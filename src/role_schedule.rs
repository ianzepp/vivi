//! Cadence / schedule health for a role seat.
//!
//! Cadence is an optional configured maximum silence interval. Health is derived
//! from the age of the role's latest outbound mailspace message (not process
//! liveness, not lifecycle events). Due is advisory; overdue requires action.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::duration::parse_duration_secs;

/// Grace on the ok → due boundary as a fraction of one cadence (10%).
const DUE_GRACE_NUM: u64 = 1;
const DUE_GRACE_DEN: u64 = 10;

/// Role kinds the pump auto-runs on their configured cadence.
///
/// Shared here so the mailspace layer (`step`), the CLI glue (`role`, pool
/// doctor) and the pump all read one predicate without a mailspace → pump
/// import.
pub const AUTO_RUN_KINDS: [&str; 2] = ["head", "scoper"];

/// True when a role kind is one the pump auto-runs (case-insensitive,
/// trimmed). Missing or empty kinds are never auto-run.
#[must_use]
pub fn auto_run_kind(kind: Option<&str>) -> bool {
    let Some(value) = kind.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    AUTO_RUN_KINDS
        .iter()
        .any(|known| value.eq_ignore_ascii_case(known))
}

/// Latest durable outbound signal used for schedule math.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastSignal {
    pub at: DateTime<Utc>,
    pub handle: String,
    pub local_role: String,
}

/// Schedule classification for a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleState {
    /// No cadence configured.
    None,
    /// Cadence set, but no outbound message found.
    Never,
    /// Last signal younger than one cadence (with grace).
    Ok,
    /// Silence between one and two cadences.
    Due,
    /// Silence at or beyond two cadences.
    Overdue,
}

/// Derived schedule report for `role status` and `board`.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleReport {
    pub state: ScheduleState,
    /// True when the role has exceeded two full cadence intervals.
    pub action_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cadence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cadence_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_signal_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_signal_handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_signal_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,
}

/// Evaluate schedule health from optional cadence text and an optional last signal.
///
/// Invalid cadence strings yield [`ScheduleState::None`] (callers should validate
/// on write so stored values always parse).
#[must_use]
pub fn evaluate(
    cadence: Option<&str>,
    last_signal: Option<&LastSignal>,
    now: DateTime<Utc>,
) -> ScheduleReport {
    let Some(raw) = cadence.map(str::trim).filter(|value| !value.is_empty()) else {
        return none_report();
    };
    let Ok(cadence_seconds) = parse_duration_secs(raw) else {
        return none_report();
    };
    let Some(signal) = last_signal else {
        return ScheduleReport {
            state: ScheduleState::Never,
            action_required: false,
            cadence: Some(raw.to_string()),
            cadence_seconds: Some(cadence_seconds),
            last_signal_at: None,
            last_signal_handle: None,
            last_signal_kind: None,
            age_seconds: None,
        };
    };
    let age_seconds = age_secs(signal.at, now);
    let state = classify(age_seconds, cadence_seconds);
    ScheduleReport {
        state,
        action_required: matches!(state, ScheduleState::Overdue),
        cadence: Some(raw.to_string()),
        cadence_seconds: Some(cadence_seconds),
        last_signal_at: Some(signal.at.to_rfc3339()),
        last_signal_handle: Some(signal.handle.clone()),
        last_signal_kind: Some(signal.local_role.clone()),
        age_seconds: Some(age_seconds),
    }
}

/// Human label for schedule state (text output).
#[must_use]
pub fn state_label(state: ScheduleState) -> &'static str {
    match state {
        ScheduleState::None => "none",
        ScheduleState::Never => "never",
        ScheduleState::Ok => "ok",
        ScheduleState::Due => "due",
        ScheduleState::Overdue => "overdue",
    }
}

fn none_report() -> ScheduleReport {
    ScheduleReport {
        state: ScheduleState::None,
        action_required: false,
        cadence: None,
        cadence_seconds: None,
        last_signal_at: None,
        last_signal_handle: None,
        last_signal_kind: None,
        age_seconds: None,
    }
}

fn classify(age_seconds: u64, cadence_seconds: u64) -> ScheduleState {
    let due_after = cadence_seconds.saturating_add(cadence_seconds / DUE_GRACE_DEN * DUE_GRACE_NUM);
    let overdue_after = cadence_seconds.saturating_mul(2);
    if age_seconds >= overdue_after {
        ScheduleState::Overdue
    } else if age_seconds >= due_after {
        ScheduleState::Due
    } else {
        ScheduleState::Ok
    }
}

fn age_secs(signal_at: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    now.signed_duration_since(signal_at)
        .num_seconds()
        .max(0)
        .cast_unsigned()
}

#[cfg(test)]
#[path = "role_schedule_test.rs"]
mod tests;
