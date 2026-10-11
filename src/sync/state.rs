//! Pull state: the read cursor and replies still waiting for their parent,
//! in `<project>/.vivi/sync/state.json`.
//!
//! The file belongs to one server project. A cursor from another project
//! would silently skip items, so a state recorded for a different project
//! reads as empty.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;

const STATE_FILE: &str = "state.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct SyncState {
    /// The project the cursor belongs to.
    #[serde(default)]
    pub project: String,
    /// The highest sequence handled; the next read asks for items after it.
    #[serde(default)]
    pub since: u64,
    /// Replies held without a parent: child `content_id` -> the RFC
    /// `Message-ID` it answers. Retried on every pull.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dangling: BTreeMap<String, String>,
}

impl SyncState {
    /// Load the state for `project`. A missing file, or one recorded for
    /// another project, is the empty state.
    ///
    /// # Errors
    /// Returns an error when the file exists but cannot be read or parsed.
    pub fn load(dir: &Path, project: &str) -> Result<Self, VivariumError> {
        let empty = Self {
            project: project.to_string(),
            ..Self::default()
        };
        let path = state_path(dir);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(empty),
            Err(e) => return Err(e.into()),
        };
        let state: Self = serde_json::from_str(&raw).map_err(|e| {
            VivariumError::Message(format!("sync state {} is unreadable: {e}", path.display()))
        })?;
        Ok(if state.project == project {
            state
        } else {
            empty
        })
    }

    /// Write the state atomically (temp file, then rename), creating the
    /// sync dir with a self-gitignore on first use.
    ///
    /// # Errors
    /// Returns an error when the directory or file cannot be written.
    pub fn save(&self, dir: &Path) -> Result<(), VivariumError> {
        ensure_sync_dir(dir)?;
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| VivariumError::Other(format!("failed to encode sync state: {e}")))?;
        let path = state_path(dir);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, raw)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join(STATE_FILE)
}

/// Create the sync dir (0700) with a self-gitignore so nothing under it can
/// ever be committed.
fn ensure_sync_dir(dir: &Path) -> Result<(), VivariumError> {
    crate::store::secure_create_dir_all(dir)?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "*\n")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
