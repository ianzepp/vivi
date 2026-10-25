//! The mind's own state file, `<mind home>/state.json`.
//!
//! Kept apart from every pump's `state.extra` on purpose: the mind must
//! survive a pump whose database is wedged, and a pump must never read
//! mind state. Written atomically (temp file and rename).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::VivariumError;

use super::authority::Ledger;
use super::wake::WakeRecord;

/// Recent wake start times kept for the rolling cap.
const RECENT_KEEP_SECS: u64 = 3600;

/// Everything the mind persists between restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MindState {
    /// The one pi session the mind resumes. Set on first start.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Operator pause: no autonomous wakes, no mutating actions.
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub last_wake: Option<WakeRecord>,
    /// Start times of wakes inside the last hour.
    #[serde(default)]
    pub recent_wakes: Vec<u64>,
    /// Per-need action ledger for the unstick budgets.
    #[serde(default)]
    pub ledger: Ledger,
    /// Registered roots the operator told the mind not to watch.
    #[serde(default)]
    pub ignored: BTreeSet<PathBuf>,
    /// The gate's verdicts on the last wake's envelope, told to the next wake.
    #[serde(default)]
    pub pending_report: Option<String>,
}

impl MindState {
    /// Note a wake start and forget wakes older than an hour.
    pub fn note_wake(&mut self, now: u64) {
        self.recent_wakes
            .retain(|at| now.saturating_sub(*at) < RECENT_KEEP_SECS);
        self.recent_wakes.push(now);
    }
}

/// Load state; a missing file is the default state.
///
/// # Errors
/// Fails on an unreadable or invalid file.
pub fn load_at(path: &Path) -> Result<MindState, VivariumError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MindState::default());
        }
        Err(error) => return Err(error.into()),
    };
    serde_json::from_str(&text)
        .map_err(|error| VivariumError::Parse(format!("invalid {}: {error}", path.display())))
}

/// Save state atomically.
///
/// # Errors
/// Fails when the file cannot be written.
pub fn save_at(path: &Path, state: &MindState) -> Result<(), VivariumError> {
    let parent = path
        .parent()
        .ok_or_else(|| VivariumError::Config("mind state path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| VivariumError::Parse(format!("could not encode mind state: {error}")))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, text)?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// Load, change, and save state.
///
/// # Errors
/// As [`load_at`], [`save_at`], or `change`.
pub fn update_at<T>(
    path: &Path,
    change: impl FnOnce(&mut MindState) -> Result<T, VivariumError>,
) -> Result<T, VivariumError> {
    // The service and the CLI (ignore lists, offline pause) both update the
    // file; the lock keeps one from overwriting the other's change.
    let _lock = crate::registry::lock_file_waiting(
        &path.with_extension("json.lock"),
        "mind state is being updated by another process",
    )?;
    let mut state = load_at(path)?;
    let value = change(&mut state)?;
    save_at(path, &state)?;
    Ok(value)
}

#[cfg(test)]
#[path = "store_test.rs"]
mod tests;
