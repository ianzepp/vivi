//! Pull state: the read cursor and replies still waiting for their parent,
//! in `<project>/.vivi/sync/state.json`.
//!
//! The file also records the **floor**: where this mailspace's history
//! starts, so a suffix of the project's history is never mistaken for all of
//! it. The file belongs to one server project. A cursor from another project
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
    /// Where this mailspace's history starts. Absent in a file written
    /// before windows existed: the floor is then unknown, not "full".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<Floor>,
    /// The server log's epoch this cursor was read under, when the server
    /// sends one (see the wire contract on `Page::epoch`). Absent until the
    /// first page carries an epoch; a changed epoch means the log restarted
    /// and the cursor no longer names the same history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u32>,
    /// Replies held without a parent: child `content_id` -> the RFC
    /// `Message-ID` it answers. Retried on every pull.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dangling: BTreeMap<String, String>,
}

/// The start of the history this mailspace holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Floor {
    /// Sequence of the first record received; 0 for full history.
    #[serde(default)]
    pub sequence: u64,
    /// Its `appended_at`, or the requested time when the window was empty;
    /// absent for full history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

impl Floor {
    /// The floor of a pull from the very beginning.
    #[must_use]
    pub fn full() -> Self {
        Self::default()
    }

    /// Whether this is the whole history.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.sequence == 0 && self.time.is_none()
    }
}

impl SyncState {
    /// The `vivi sync status` line for the floor.
    #[must_use]
    pub fn history_line(&self) -> String {
        let Some(floor) = &self.floor else {
            return if self.since > 0 {
                "history from: unknown (pulled before windows)".into()
            } else {
                "history from: nothing pulled yet".into()
            };
        };
        match (floor.sequence, floor.time.as_deref()) {
            (0, None) => "history from: full history".into(),
            (0, Some(time)) => format!("history from: {time} (no letters yet)"),
            (sequence, Some(time)) => format!("history from: {time} (sequence {sequence})"),
            (sequence, None) => format!("history from: sequence {sequence}"),
        }
    }

    /// The `vivi sync status` line for the epoch.
    #[must_use]
    pub fn epoch_line(&self) -> String {
        match self.epoch {
            Some(epoch) => format!("epoch: {epoch}"),
            None => "epoch: not sent by the server".into(),
        }
    }

    /// Load the state for `project`. A missing file, or one recorded for
    /// another project, is the empty state.
    ///
    /// # Errors
    /// Returns an error when the file exists but cannot be read or parsed.
    /// The message names the file and the command that rebuilds it.
    pub fn load(dir: &Path, project: &str) -> Result<Self, VivariumError> {
        let empty = Self {
            project: project.to_string(),
            ..Self::default()
        };
        let path = state_path(dir);
        let unreadable = |cause: &str| {
            VivariumError::Message(format!(
                "sync state {} {cause}; run `vivi sync resync` to rebuild it",
                path.display()
            ))
        };
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(empty),
            Err(e) => return Err(unreadable(&format!("cannot be read ({e})"))),
        };
        let state: Self = serde_json::from_str(&raw)
            .map_err(|_| unreadable("is corrupt (it is not a valid sync state)"))?;
        Ok(if state.project == project {
            state
        } else {
            empty
        })
    }

    /// Load the state a resync starts from: whatever the file holds, or the
    /// empty state when it cannot be read. A resync resets the cursor and
    /// floor anyway and writes the file fresh, so an unreadable file is the
    /// very thing it repairs.
    #[must_use]
    pub fn load_for_resync(dir: &Path, project: &str) -> Self {
        Self::load(dir, project).unwrap_or_else(|_| Self {
            project: project.to_string(),
            ..Self::default()
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
pub(crate) fn ensure_sync_dir(dir: &Path) -> Result<(), VivariumError> {
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
