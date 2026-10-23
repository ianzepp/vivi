//! Guarded writes: refuse while the pump runs, keep a backup, and roll a write
//! back when it makes the project's configuration worse.
//!
//! A write is "worse" when the project's readiness assessment has an error
//! after the write that it did not have before. Errors that were already there
//! are reported (not blamed on the write); not-ready items and warnings never
//! count.
#![allow(clippy::missing_errors_doc)]

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

use crate::error::VivariumError;
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR, Mailspace};
use crate::pump::control::{self, PumpMode};
use crate::routing::Severity;

/// How many timestamped backups of one file are kept.
const KEEP_BACKUPS: usize = 10;

/// What a guarded write left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedWrite {
    /// The backup file made before the write, when the file existed.
    pub backup: Option<PathBuf>,
    /// Errors the project already had before the write and still has.
    pub still: Vec<String>,
}

impl GuardedWrite {
    /// Lines for a CLI to print on stderr: the backup, then each old error.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(path) = &self.backup {
            notes.push(format!("backup {}", path.display()));
        }
        notes.extend(self.still.iter().map(|m| format!("still invalid: {m}")));
        notes
    }
}

/// Edit `<root>/.vivi/mailspace.toml` under the guard.
pub fn guarded(
    root: &Path,
    edit: impl FnOnce(&mut DocumentMut) -> Result<(), VivariumError>,
) -> Result<GuardedWrite, VivariumError> {
    let file = root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
    let ((), outcome) = guard_file(root, &file, || super::apply(&file, edit))?;
    Ok(outcome)
}

/// Run `write` (which changes `file`) under the guard.
pub fn guard_file<T>(
    root: &Path,
    file: &Path,
    write: impl FnOnce() -> Result<T, VivariumError>,
) -> Result<(T, GuardedWrite), VivariumError> {
    // Open first: opening may mint the id and write `.vivi/.gitignore`, and a
    // rollback must not undo those.
    let (before, home) = snapshot_state(root);
    refuse_while_running(&home)?;
    let snapshot = read_snapshot(file)?;
    let backup = match &snapshot {
        Some(bytes) => Some(make_backup(file, bytes)?),
        None => None,
    };
    let value = write()?;
    let (after, _) = snapshot_state(root);
    let new: Vec<String> = after
        .iter()
        .filter(|message| !before.contains(message))
        .cloned()
        .collect();
    if !new.is_empty() {
        restore(file, snapshot.as_deref())?;
        return Err(VivariumError::Config(format!(
            "the write was rolled back: it adds new errors: {}",
            new.join("; ")
        )));
    }
    Ok((
        value,
        GuardedWrite {
            backup,
            still: after,
        },
    ))
}

/// The error messages of the project as it stands, and the pump home.
fn snapshot_state(root: &Path) -> (Vec<String>, PathBuf) {
    let default_home = root.join(MAILSPACE_DIR).join("pump");
    match Mailspace::open_root(root.to_path_buf()) {
        Ok(mailspace) => {
            let errors = crate::pump::readiness::assess(&mailspace)
                .into_iter()
                .filter(|issue| issue.severity == Severity::Error)
                .map(|issue| issue.message)
                .collect();
            let home = mailspace
                .config
                .pump
                .as_ref()
                .map_or_else(PathBuf::new, |cfg| cfg.home_dir(root));
            (errors, home)
        }
        Err(err) => (vec![err.to_string()], default_home),
    }
}

/// Refuse when a pump process holds the lock and the stored mode is running.
/// An empty `home` means the project has no `[pump]`: nothing to check.
fn refuse_while_running(home: &Path) -> Result<(), VivariumError> {
    if home.as_os_str().is_empty() {
        return Ok(());
    }
    if crate::pump::run::lock_is_held(home)? && control::inspect(home)?.mode == PumpMode::Running {
        return Err(VivariumError::Config(
            "the pump is running: run vivi pump pause first".into(),
        ));
    }
    Ok(())
}

fn read_snapshot(file: &Path) -> Result<Option<Vec<u8>>, VivariumError> {
    match fs::read(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn make_backup(file: &Path, bytes: &[u8]) -> Result<PathBuf, VivariumError> {
    let name = file_name(file);
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%6fZ");
    let backup = file.with_file_name(format!("{name}.bak.{stamp}"));
    fs::write(&backup, bytes)?;
    prune(file, &name)?;
    Ok(backup)
}

fn file_name(file: &Path) -> String {
    file.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// Keep the newest [`KEEP_BACKUPS`] timestamped backups. Other `.bak.` files
/// (those that do not end in `Z`) belong to other tools and stay.
fn prune(file: &Path, name: &str) -> Result<(), VivariumError> {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let prefix = format!("{name}.bak.");
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let found = entry?.file_name().to_string_lossy().into_owned();
        if found.starts_with(&prefix) && found.ends_with('Z') {
            names.push(found);
        }
    }
    names.sort();
    let excess = names.len().saturating_sub(KEEP_BACKUPS);
    for old in &names[..excess] {
        fs::remove_file(dir.join(old))?;
    }
    Ok(())
}

fn restore(file: &Path, snapshot: Option<&[u8]>) -> Result<(), VivariumError> {
    match snapshot {
        Some(bytes) => {
            let tmp = file.with_file_name(format!("{}.tmp", file_name(file)));
            fs::write(&tmp, bytes)?;
            fs::rename(&tmp, file)?;
        }
        None => match fs::remove_file(file) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        },
    }
    Ok(())
}

#[cfg(test)]
#[path = "guard_test.rs"]
mod tests;
