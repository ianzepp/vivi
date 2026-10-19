//! Pump cadence: scan role schedules and auto-run due roles on their own
//! cadence, with one in-flight run per role and backoff on failures.
//!
//! The core here is pure: scans are rows of `vivi role list --json`, schedule
//! state comes from `vivi role status <name> --json`, and the persisted state
//! rides in `state.extra` (which `pump.db` already persists) like
//! [`crate::pump::audit`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pump::state::State;
use crate::role_schedule::auto_run_kind;

/// The `state.extra` key holding the cadence state.
pub const EXTRA_KEY: &str = "cadence";

/// Role statuses that disable auto-run: a paused/retired role is not run.
/// The codebase's preferred term is `parked`; the operator language names
/// `paused`/`retired`, so all three count.
pub const DISABLED_STATUSES: [&str; 3] = ["paused", "parked", "retired"];

/// Seconds between role-schedule scans.
pub const SCAN_EVERY: u64 = 60;

/// Most roles with open wants the pump keeps scanning for at once.
pub const MAX_OPEN_WANTS: usize = 8;

/// First-failure backoff in seconds; doubles per attempt, capped (see
/// [`backoff_secs`]).
pub const BACKOFF_BASE: u64 = 60;

/// One scanned role row of `vivi role list --json` (the [`crate::mailspace`]
/// `RoleView` fields the pump cares about).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleScan {
    /// The role name.
    #[serde(default)]
    pub name: String,
    /// The role kind (head, hand, …); `None` when unset.
    #[serde(default)]
    pub kind: Option<String>,
    /// The role status (active, paused, …).
    #[serde(default)]
    pub status: String,
    /// The role's cadence text (e.g. `4h`); `None` when no cadence is set.
    #[serde(default)]
    pub cadence: Option<String>,
    /// The role's pool; `None` when unset.
    #[serde(default)]
    pub pool: Option<String>,
}

/// One cadence run record for a role.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRun {
    /// The pump task handle for the run.
    #[serde(default)]
    pub task: String,
    /// Epoch seconds the run started.
    #[serde(default)]
    pub started_at: u64,
    /// Epoch seconds the run ended; `None` while the run is in flight.
    #[serde(default)]
    pub ended_at: Option<u64>,
    /// `done` | `failed` | `timed_out` | `running`.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Attempt number for this run.
    #[serde(default)]
    pub attempt: u32,
    /// Epoch seconds before which a retry may not start.
    #[serde(default)]
    pub retry_at: Option<u64>,
}

/// Persisted cadence state: per-role run records and last scan rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CadenceState {
    /// Run records keyed by role name.
    #[serde(default)]
    pub runs: BTreeMap<String, RoleRun>,
    /// Last scan rows keyed by role name.
    #[serde(default)]
    pub scans: BTreeMap<String, RoleScan>,
    /// Epoch seconds of the last role scan.
    #[serde(default)]
    pub scanned_at: u64,
}

/// Read the cadence state from `state.extra`; a missing or unreadable
/// payload decodes to the default.
#[must_use]
pub fn state(st: &State) -> CadenceState {
    let Some(value) = st.extra.get(EXTRA_KEY) else {
        return CadenceState::default();
    };
    match serde_json::from_value(value.clone()) {
        Ok(decoded) => decoded,
        Err(_) => CadenceState::default(),
    }
}

/// Write the cadence state into `state.extra` (same persistence pattern as
/// the audit run).
pub fn set_state(st: &mut State, cadence: &CadenceState) {
    match serde_json::to_value(cadence) {
        Ok(value) => {
            st.extra.insert(String::from(EXTRA_KEY), value);
        }
        Err(_) => {
            st.extra.remove(EXTRA_KEY);
        }
    }
}

/// Parse the JSON array of `vivi role list --json` into scans, skipping any
/// text before the first `[` (same tolerance as [`crate::pump::board`]).
/// Unreadable output yields no rows.
#[must_use]
pub fn parse_role_rows(out: &str) -> Vec<RoleScan> {
    let Some(start) = out.find('[') else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<RoleScan>>(&out[start..]) {
        Ok(rows) => rows,
        Err(_) => Vec::new(),
    }
}

/// Parse `vivi role status <name> --json` into `(schedule state, age
/// seconds)`. Unreadable output yields `None`.
#[must_use]
pub fn parse_schedule(out: &str) -> Option<(String, Option<u64>)> {
    let value = match serde_json::from_str::<Value>(out) {
        Ok(value) => value,
        Err(_) => return None,
    };
    let schedule = value.get("schedule")?;
    let state = schedule.get("state")?.as_str()?.to_owned();
    let age = schedule.get("age_seconds").and_then(Value::as_u64);
    Some((state, age))
}

/// When is the role next due? `None` means due now: states `due`,
/// `overdue` and `never` are due immediately. For `ok` the role is due at
/// its last signal plus one cadence plus the 10% grace
/// [`crate::role_schedule`] applies on the ok → due boundary; a due time at
/// or before `now` saturates to due now. Unknown states are due now.
#[must_use]
pub fn next_due(
    now: u64,
    state: &str,
    cadence_seconds: u64,
    age_seconds: Option<u64>,
) -> Option<u64> {
    if state != "ok" {
        return None;
    }
    let age = age_seconds?;
    let grace = cadence_seconds / 10;
    let last_signal = now.saturating_sub(age);
    let due = last_signal
        .saturating_add(cadence_seconds)
        .saturating_add(grace);
    (due > now).then_some(due)
}

/// True when the pump may launch a cadence run for this role now: the role
/// has a cadence, its kind is one [`crate::role_schedule::AUTO_RUN_KINDS`],
/// its status is not disabled (case-insensitive), it has no open run, and
/// any pending retry is due.
#[must_use]
pub fn launchable(scan: &RoleScan, run: Option<&RoleRun>, now: u64) -> bool {
    let cadence_set = scan
        .cadence
        .as_deref()
        .is_some_and(|cadence| !cadence.trim().is_empty());
    let kind_ok = auto_run_kind(scan.kind.as_deref());
    let status = scan.status.to_lowercase();
    let status_ok = !DISABLED_STATUSES.iter().any(|stop| status == *stop);
    let open = run.is_some_and(|run| run.ended_at.is_none());
    let retry_pending = run.is_some_and(|run| run.retry_at.is_some_and(|at| now < at));
    cadence_set && kind_ok && status_ok && !open && !retry_pending
}

/// Retry backoff in seconds: [`BACKOFF_BASE`] shifted left once per prior
/// attempt, capped at `1 << 5` multiples so the wait stays bounded.
#[must_use]
pub fn backoff_secs(attempt: u32) -> u64 {
    let shift = u32::min(attempt.saturating_sub(1), 5);
    BACKOFF_BASE << shift
}

#[cfg(test)]
#[path = "cadence_test.rs"]
mod tests;
