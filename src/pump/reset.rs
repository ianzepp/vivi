//! `vivi pump reset`: archive the pump home beside itself and start fresh.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::VivariumError;
use crate::mailspace::{Mailspace, PumpState};

use super::control::{self, PumpMode};

/// Home entries reset moves into the archive; missing entries are skipped.
const ARCHIVE_ENTRIES: [&str; 10] = [
    "pump.db",
    "pump.db-wal",
    "pump.db-shm",
    "state.json",
    "state.json.imported",
    "plans",
    "resplit",
    "reserved.txt",
    "exceptions.txt",
    "status.txt",
];

/// What one reset did; the whole input of the CLI printer.
#[derive(Debug, Clone)]
pub struct ResetReport {
    /// The `pump-archive-<UTC stamp>` directory the entries moved into.
    pub archive_dir: PathBuf,
    /// Home-relative names of the entries that were present and moved.
    pub moved: Vec<String>,
    /// Control mode of the fresh database.
    pub mode: PumpMode,
    /// Open pump records (letters) the next tick re-ingests.
    pub open_records: usize,
}

/// Archive the pump home beside itself and initialize a fresh database.
///
/// Refused while the pump lock is held (a paused pump holds it too); on
/// refusal nothing on disk changes. `targets/`, `merge-lanes/`,
/// `sync-push.log`, `lock` and `log.txt` stay in place, and `log.txt` gains
/// one line naming the archive. The fresh database seeds control `Stopped`.
///
/// # Errors
/// Discovery, the lock probe, the renames, the log append, and the fresh
/// database can all fail.
pub fn archive(project: &Path) -> Result<ResetReport, VivariumError> {
    let home = control::project_home(project)?;
    if super::run::lock_is_held(&home)? {
        return Err(VivariumError::Message(String::from(
            "the pump is running (lock held): run vivi pump stop and wait for it to exit, then reset",
        )));
    }
    let archive_dir = archive_dir(&home)?;
    fs::create_dir_all(&archive_dir)?;
    fs::create_dir_all(&home)?;
    let moved = move_entries(&home, &archive_dir)?;
    append_log_line(&home, &archive_dir)?;
    let control = control::initialize(&home)?;
    let open_records = Mailspace::discover(Some(project))?
        .pump_list(None, Some(PumpState::Open))?
        .len();
    Ok(ResetReport {
        archive_dir,
        moved,
        mode: control.mode,
        open_records,
    })
}

/// `<home>/../pump-archive-<UTC stamp>`: beside the pump home, so a rename
/// never crosses a filesystem.
fn archive_dir(home: &Path) -> Result<PathBuf, VivariumError> {
    let parent = home.parent().ok_or_else(|| {
        VivariumError::Message(format!(
            "pump home {} has no parent directory",
            home.display()
        ))
    })?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    Ok(parent.join(format!("pump-archive-{stamp}")))
}

/// Rename every present home entry into the archive; name the moved ones.
fn move_entries(home: &Path, archive_dir: &Path) -> Result<Vec<String>, VivariumError> {
    let mut moved = Vec::new();
    for entry in ARCHIVE_ENTRIES {
        let from = home.join(entry);
        if fs::symlink_metadata(&from).is_err() {
            continue;
        }
        fs::rename(&from, archive_dir.join(entry))?;
        moved.push(String::from(entry));
    }
    Ok(moved)
}

/// Append the reset line to `log.txt`, creating it when missing.
fn append_log_line(home: &Path, archive_dir: &Path) -> Result<(), VivariumError> {
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("log.txt"))?;
    writeln!(log, "reset: archived to {}", archive_dir.display())?;
    Ok(())
}
