//! Cadence: which cadenced roles the pump auto-runs, and their run receipts.
//!
//! A role's cadence is advisory for humans (`vivi role status`), but the pump
//! acts on it: a role whose kind is in [`role_schedule::AUTO_RUN_KINDS`] is
//! started when its schedule comes due, then tracked by a run receipt in
//! `state.extra` so a slow or crashed seat backs off instead of piling up.
//! Receipts persist per the `audit.rs` pattern (`pump.db` already keeps
//! `state.extra`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pump::state::State;
use crate::role_schedule;

/// The `state.extra` key holding cadence receipts.
pub const EXTRA_KEY: &str = "cadence";

/// Role statuses that disable auto-run. "paused" and "retired" come from the
/// need; "parked" is this codebase's preferred term and is honored too.
pub const DISABLED_STATUSES: [&str; 3] = ["paused", "parked", "retired"];

/// Seconds between role scans.
pub const SCAN_EVERY: u64 = 60;

/// Wants a single cadence run may leave open before the pump stops adding more.
pub const MAX_OPEN_WANTS: usize = 8;

/// First retry delay after a failed run, in seconds; doubles per attempt.
pub const BACKOFF_BASE: u64 = 60;

/// Grace on top of one cadence, as a fraction (10%), matching schedule health.
const DUE_GRACE_NUM: u64 = 1;
const DUE_GRACE_DEN: u64 = 10;

/// Highest exponent the retry backoff may reach (64× [`BACKOFF_BASE`]).
const BACKOFF_MAX_SHIFT: u32 = 5;

/// One row of `vivi role list --json` (`RoleView` fields the pump needs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleScan {
    /// Role name.
    #[serde(default)]
    pub name: String,
    /// Role kind (`head`, `hand`, ...); only auto-run kinds launch.
    #[serde(default)]
    pub kind: Option<String>,
    /// Lifecycle status; [`DISABLED_STATUSES`] never launch.
    #[serde(default)]
    pub status: String,
    /// Configured silence interval (`15m`); absent means no auto-run.
    #[serde(default)]
    pub cadence: Option<String>,
    /// Pool the role's units run in.
    #[serde(default)]
    pub pool: Option<String>,
}

/// One cadence run: what the pump started, and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRun {
    /// The task handle the run was dispatched as.
    #[serde(default)]
    pub task: String,
    /// Epoch seconds the run started.
    #[serde(default)]
    pub started_at: u64,
    /// Epoch seconds the run ended; `None` while it is open.
    #[serde(default)]
    pub ended_at: Option<u64>,
    /// `done`, `failed`, `timed_out`, or `running`.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Attempt count, driving [`backoff_secs`].
    #[serde(default)]
    pub attempt: u32,
    /// Epoch seconds before which the role is not restarted.
    #[serde(default)]
    pub retry_at: Option<u64>,
}

/// Cadence bookkeeping persisted in `state.extra[EXTRA_KEY]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CadenceState {
    /// Latest run per role name.
    #[serde(default)]
    pub runs: BTreeMap<String, RoleRun>,
    /// Latest scan row per role name.
    #[serde(default)]
    pub scans: BTreeMap<String, RoleScan>,
    /// Epoch seconds of the last role scan.
    #[serde(default)]
    pub scanned_at: u64,
}

/// Decode cadence state from `state.extra`; absent or invalid gives the default.
#[must_use]
pub fn state(st: &State) -> CadenceState {
    let Some(value) = st.extra.get(EXTRA_KEY) else {
        return CadenceState::default();
    };
    serde_json::from_value(value.clone()).unwrap_or_default()
}

/// Encode cadence state into `state.extra`, replacing any prior value.
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

/// Parse the JSON array of `vivi role list --json`, skipping any text before
/// the first `[`. Invalid output gives an empty scan.
#[must_use]
pub fn parse_role_rows(out: &str) -> Vec<RoleScan> {
    let Some(start) = out.find('[') else {
        return Vec::new();
    };
    let Ok(values) = serde_json::from_str::<Vec<Value>>(&out[start..]) else {
        return Vec::new();
    };
    values.iter().map(scan_from).collect()
}

/// Build a scan row from one JSON object; missing keys default empty.
fn scan_from(value: &Value) -> RoleScan {
    RoleScan {
        name: text(value, "name").unwrap_or_default(),
        kind: text(value, "kind"),
        status: text(value, "status").unwrap_or_default(),
        cadence: text(value, "cadence"),
        pool: text(value, "pool"),
    }
}

/// One string field of a JSON object, absent or non-string giving `None`.
fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(String::from)
}

/// Parse `vivi role status <name> --json` into `(schedule.state, schedule.age_seconds)`.
#[must_use]
pub fn parse_schedule(out: &str) -> Option<(String, Option<u64>)> {
    let start = out.find('{')?;
    let value = serde_json::from_str::<Value>(&out[start..]).ok()?;
    let schedule = value.get("schedule")?;
    let state = text(schedule, "state")?;
    let age = schedule.get("age_seconds").and_then(Value::as_u64);
    Some((state, age))
}

/// Next due epoch second, or `None` when the role is due now.
///
/// `due`, `overdue` and `never` are due now. `ok` is due one cadence plus 10%
/// grace after the last signal, clamped to `now` (a stale `ok` is due now).
#[must_use]
pub fn next_due(
    now: u64,
    state: &str,
    cadence_seconds: u64,
    age_seconds: Option<u64>,
) -> Option<u64> {
    if !state.trim().eq_ignore_ascii_case("ok") {
        return None;
    }
    let age = age_seconds?;
    let grace = cadence_seconds / DUE_GRACE_DEN * DUE_GRACE_NUM;
    let last_signal = now.saturating_sub(age);
    let due_at = last_signal
        .saturating_add(cadence_seconds)
        .saturating_add(grace);
    Some(due_at.max(now))
}

/// True when a scanned role may be started now.
#[must_use]
pub fn launchable(scan: &RoleScan, run: Option<&RoleRun>, now: u64) -> bool {
    if !cadence_set(scan) {
        return false;
    }
    if !role_schedule::auto_run_kind(scan.kind.as_deref()) {
        return false;
    }
    if DISABLED_STATUSES
        .iter()
        .any(|status| scan.status.trim().eq_ignore_ascii_case(status))
    {
        return false;
    }
    let Some(run) = run else {
        return true;
    };
    run.ended_at.is_some() && run.retry_at.is_none_or(|at| at <= now)
}

/// A role with a non-empty cadence is a cadence candidate.
fn cadence_set(scan: &RoleScan) -> bool {
    scan.cadence
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

/// Retry delay after a failed attempt: [`BACKOFF_BASE`] doubled per attempt,
/// capped at 32× the base.
#[must_use]
pub fn backoff_secs(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(BACKOFF_MAX_SHIFT);
    BACKOFF_BASE << shift
}

#[cfg(test)]
#[path = "cadence_test.rs"]
mod tests;
