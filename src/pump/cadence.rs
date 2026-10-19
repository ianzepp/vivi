//! Auto-run cadence state for head/scoper roles; receipts persist in state.extra.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::pump::state::State;

/// The `state.extra` key holding the cadence state.
pub const EXTRA_KEY: &str = "cadence";

/// Role statuses that disable auto-run.
pub const DISABLED_STATUSES: [&str; 3] = ["paused", "parked", "retired"];

/// Seconds between role scans.
pub const SCAN_EVERY: u64 = 60;

/// Open wants a role may hold before auto-run declines to file another.
pub const MAX_OPEN_WANTS: usize = 8;

/// Seconds of backoff for the first retry of a failed run.
pub const BACKOFF_BASE: u64 = 60;

/// One role as seen by the cadence scan.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoleScan {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub cadence: Option<String>,
    #[serde(default)]
    pub pool: Option<String>,
}

/// One auto-run of a role: which task, when, and how it ended.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RoleRun {
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub ended_at: Option<u64>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub retry_at: Option<u64>,
}

/// Cadence state: last scan time, the roles seen, and their runs.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CadenceState {
    #[serde(default)]
    pub runs: BTreeMap<String, RoleRun>,
    #[serde(default)]
    pub scans: BTreeMap<String, RoleScan>,
    #[serde(default)]
    pub scanned_at: u64,
}

/// Read the cadence state, falling back to defaults for absent or bad payloads.
pub fn state(st: &State) -> CadenceState {
    let Some(value) = st.extra.get(EXTRA_KEY).cloned() else {
        return CadenceState::default();
    };
    serde_json::from_value(value).unwrap_or_default()
}

/// Store the cadence state in `state.extra`.
pub fn set_state(st: &mut State, cadence: &CadenceState) {
    if let Ok(value) = serde_json::to_value(cadence) {
        st.extra.insert(String::from(EXTRA_KEY), value);
    }
}

/// Seconds to wait before retrying a run that has failed `attempt` times.
#[must_use]
pub fn backoff_secs(attempt: u32) -> u64 {
    BACKOFF_BASE << attempt.saturating_sub(1).min(5)
}

#[cfg(test)]
#[path = "cadence_test.rs"]
mod tests;
