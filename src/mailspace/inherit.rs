//! Mailspace config inheritance.
//!
//! A directory above a project can hold a **base**: a
//! `.vivi/mailspace.toml` with a top-level `base = true`. When a project's
//! config is loaded, every base on the way up to `/` is merged under the
//! project's own file, deeper files winning. This is the one place that turns
//! config text into a [`MailspaceConfig`], so every reader sees the merged
//! view; writers still edit only the project's own file (they never go
//! through this module).
//!
//! # Opt-in
//!
//! An ancestor without `base = true` is ignored: it is parsed only far enough
//! to read the flag, and nothing in it is used. An ancestor directory or file
//! that cannot be read ends the walk quietly, so a locked-down parent never
//! turns into an error.
//!
//! # What is inherited
//!
//! Only these top-level keys: `pump`, `pools`, `identities` (the roles),
//! `judgment`, `probes`, `absorb`, and `memo`. Never inherited: `id`, `name`,
//! `description`, `archive`, `sync`, the retired `pool` table, and `base`
//! itself. Mail, the store, rules, and the mailspace identity stay with the
//! project. A base may be config-only and hold no `mail.sqlite`.
//!
//! # Merge rules
//!
//! - Tables merge key by key, recursively; the deeper file wins a clash.
//! - Arrays and scalars replace whole, except `identities`.
//! - `identities` merge by role `name`: a role the project also defines is
//!   merged key by key (arrays inside still replace), other base roles stay.
//!
//! # Removing an inherited key: `unset`
//!
//! A file may carry a top-level list of dotted key paths:
//!
//! ```toml
//! unset = ["pools.studio.size_gib", "pools.old", "identities.reviewer"]
//! ```
//!
//! Each path is removed from what the files **above** this one provided,
//! before this file's own keys apply, so a file can unset a key and set it
//! again. A segment that contains a dot is quoted: `pools."a.b".region`.
//! Inside `identities`, a segment names a role. A path that matches nothing is
//! reported by `vivi config validate`, not an error.
//!
//! # Charters
//!
//! `.vivi/charters/<role>.md` is read from the project first, then from the
//! nearest base that has it; see [`charter_file`].

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use super::{MAILSPACE_CONFIG, MAILSPACE_DIR, MailspaceConfig};
use crate::error::VivariumError;

mod merge;
mod origins;

pub use origins::{ConfigOrigins, OriginEntry, UnsetRecord};

/// Top-level keys a base may supply.
pub const INHERITED_KEYS: [&str; 7] = [
    "pump",
    "pools",
    "identities",
    "judgment",
    "probes",
    "absorb",
    "memo",
];

pub(crate) fn is_inherited_key(key: &str) -> bool {
    INHERITED_KEYS.contains(&key)
}

/// The merged config and where its parts came from.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: MailspaceConfig,
    pub origins: ConfigOrigins,
}

/// One config file taking part in a merge.
pub(crate) struct Layer {
    pub(crate) path: PathBuf,
    pub(crate) table: Table,
}

/// Parse the project config at `path` (whose text is `raw`), merged over its
/// bases.
///
/// # Errors
/// Returns an error when the project file does not parse, a base's `unset`
/// is malformed, or the merged result is not a valid config.
pub fn load_config(path: &Path, raw: &str) -> Result<MailspaceConfig, VivariumError> {
    Ok(load_with_origins(path, raw)?.config)
}

/// Like [`load_config`], also returning where each part came from.
///
/// # Errors
/// As [`load_config`].
pub fn load_with_origins(path: &Path, raw: &str) -> Result<LoadedConfig, VivariumError> {
    let project = parse_table(path, raw)?;
    let (mut layers, skipped) = collect_bases(&project_root(path));
    layers.push(Layer {
        path: path.to_path_buf(),
        table: project,
    });
    let (merged, origins) = merge::merge_layers(layers, skipped)?;
    let config = Value::Table(merged).try_into().map_err(|e| {
        VivariumError::Message(format!(
            "failed to parse mailspace config at {}: {e}",
            path.display()
        ))
    })?;
    Ok(LoadedConfig { config, origins })
}

/// Whether the mailspace at `root` marks itself a base. A missing or
/// unparseable file is not a base.
#[must_use]
pub fn is_base_root(root: &Path) -> bool {
    let path = root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
    match fs::read_to_string(path) {
        Ok(text) => matches!(text.parse::<Table>(), Ok(table) if marks_base(&table)),
        Err(_) => false,
    }
}

/// Refuse a root that is a base: a base is configuration for other
/// projects, never a project to register or pump.
///
/// # Errors
/// Returns an error naming the root when its mailspace has `base = true`.
pub fn refuse_base(root: &Path) -> Result<(), VivariumError> {
    if is_base_root(root) {
        return Err(VivariumError::Config(format!(
            "{} is a base config (`base = true` in .vivi/mailspace.toml), not a project; \
             it cannot be registered or pumped",
            root.display()
        )));
    }
    Ok(())
}

/// The charter directories of the bases above `root`, nearest first.
#[must_use]
pub fn base_charter_dirs(root: &Path) -> Vec<PathBuf> {
    let (layers, _) = collect_bases(root);
    layers
        .iter()
        .rev()
        .filter_map(|layer| layer.path.parent().map(|dir| dir.join("charters")))
        .collect()
}

/// The charter file to read for `name` in the mailspace whose `.vivi`
/// directory is `vivi_dir`: the project's own, else the nearest base's.
/// When neither exists this is the project's path, which is where a new
/// charter is written.
#[must_use]
pub fn charter_file(vivi_dir: &Path, name: &str) -> PathBuf {
    let file = format!("{name}.md");
    let own = vivi_dir.join("charters").join(&file);
    if own.exists() {
        return own;
    }
    let root = vivi_dir.parent().unwrap_or(vivi_dir);
    base_charter_dirs(root)
        .into_iter()
        .map(|dir| dir.join(&file))
        .find(|path| path.exists())
        .unwrap_or(own)
}

fn marks_base(table: &Table) -> bool {
    table.get("base").and_then(Value::as_bool) == Some(true)
}

fn project_root(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn parse_table(path: &Path, raw: &str) -> Result<Table, VivariumError> {
    raw.parse::<Table>().map_err(|e| {
        VivariumError::Message(format!(
            "failed to parse mailspace config at {}: {e}",
            path.display()
        ))
    })
}

/// The bases above `root`, farthest first, and the ancestors that could not
/// be parsed. The walk stops at the first ancestor file that exists but
/// cannot be read.
fn collect_bases(root: &Path) -> (Vec<Layer>, Vec<(PathBuf, String)>) {
    let mut layers = Vec::new();
    let mut skipped = Vec::new();
    let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    for dir in root.ancestors().skip(1) {
        let path = dir.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                continue;
            }
            Err(_) => break,
        };
        match text.parse::<Table>() {
            Ok(table) if marks_base(&table) => layers.push(Layer { path, table }),
            Ok(_) => {}
            Err(e) => skipped.push((path, e.to_string())),
        }
    }
    layers.reverse();
    (layers, skipped)
}

#[cfg(test)]
#[path = "inherit_test.rs"]
mod tests;
