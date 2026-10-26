//! Row vocabulary shared by every monitor surface: what one running lane
//! looks like in a snapshot, plus small formatting helpers over it.

use std::path::PathBuf;

/// Whether the row is the pump service itself or one supervised slot.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RowKind {
    Pump,
    Slot,
}

/// Lifecycle state of a lane: still running, or finished with an outcome word
/// (e.g. `pushed`, `failed`, `timed out`, `uncollected`).
///
/// Serde shape: `Running` serializes as `"running"`; `Finished(w)` as
/// `{"finished": w}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RowState {
    Running,
    Finished(String),
}

/// One observed lane: the pump, or a slot running one role for one mailspace.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MonitorRow {
    /// Mailspace name the lane works for.
    pub project: String,
    pub kind: RowKind,
    pub pool: String,
    pub slot: String,
    /// Where the worker runs, e.g. `local`, `ssh`, `pump`.
    pub provider: String,
    /// Task addressee role; empty when unknown.
    pub role: String,
    /// Task handle for lanes or stable cycle identity for pump rows.
    pub task: Option<String>,
    /// Parent need handle, when the task or pool record names one.
    pub need: Option<String>,
    /// Work kind carried by the task or pool record, when known.
    pub work_kind: Option<String>,
    pub subject: Option<String>,
    pub model: String,
    pub state: RowState,
    /// Epoch seconds of recorded completion, when the lane finished and the
    /// completion time is known; absent while running.
    pub finished_at: Option<u64>,
    /// Age in seconds since completion once the recent-finished window has
    /// lapsed; fresh rows have no stale age.
    pub stale_age_secs: Option<u64>,
    /// Epoch seconds the lane started.
    pub started_at: Option<u64>,
    pub running_secs: Option<u64>,
    pub budget_secs: Option<u64>,
    pub read_only: Option<bool>,
    pub remote: bool,
    /// Path of the lane's live log tail, when one exists.
    pub tail: Option<PathBuf>,
}

/// Whether the lane has finished, whatever the outcome was.
pub fn is_finished(row: &MonitorRow) -> bool {
    matches!(row.state, RowState::Finished(_))
}

/// Shared recency window for finished rows: a completion no older than this
/// many seconds counts as recently finished.
pub const RECENT_FINISHED_SECS: u64 = 300;

/// Whether the row completed within [`RECENT_FINISHED_SECS`] of `now`,
/// inclusive at exactly the window boundary. Recency is determined from the
/// recorded completion; an unavailable completion cannot establish recency.
pub fn recently_finished(row: &MonitorRow, now: u64) -> bool {
    row.finished_at
        .is_some_and(|finished_at| now.saturating_sub(finished_at) <= RECENT_FINISHED_SECS)
}

/// Execution identity of one lane across snapshots, for session history.
/// Kind and start time separate retries of the same task in the same slot;
/// freshness, runtime, and lifecycle changes leave the key unchanged.
pub fn activity_key(row: &MonitorRow) -> String {
    format!(
        "{}|{:?}|{}|{}|{:?}|{:?}",
        row.project, row.kind, row.pool, row.slot, row.task, row.started_at
    )
}

/// Human duration: `45s`, `12m30s` (or `12m` with no remainder), `1h23m`.
pub fn format_secs(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        let mins = secs / 60;
        let rem = secs % 60;
        return if rem == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m{rem}s")
        };
    }
    format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
}

/// `<running>/<budget>`; budget unknown shows running alone, both unknown `-`.
pub fn running_text(row: &MonitorRow) -> String {
    match (row.running_secs, row.budget_secs) {
        (running, None) => running.map_or("-".to_string(), format_secs),
        (None, Some(budget)) => format!("-/{}", format_secs(budget)),
        (Some(running), Some(budget)) => {
            format!("{}/{}", format_secs(running), format_secs(budget))
        }
    }
}

/// Local wall-clock start as UTC `HH:MM`, or `-` when unknown.
pub fn started_text(epoch: Option<u64>) -> String {
    epoch
        .and_then(|secs| chrono::DateTime::from_timestamp(secs as i64, 0))
        .map_or_else(|| "-".to_string(), |t| t.format("%H:%M").to_string())
}

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
