//! When the mind wakes.
//!
//! Two triggers: a heartbeat, and pump signals. The heartbeat reuses the
//! role schedule math ([`role_schedule::evaluate`]): the mind is treated as
//! a seat whose last durable signal is its last finished wake, so `due` and
//! `overdue` mean the same thing they mean for a cadenced role. A failed
//! wake backs off with the cadence backoff ([`cadence::backoff_secs`]), and
//! a wake in flight blocks the next one, as `cadence::launchable` does for
//! role runs. Pump signals wake the mind only when they differ from what the
//! last wake already saw, so a pump that stays stalled is re-examined on the
//! heartbeat rather than on every tick. A rolling-hour cap bounds both.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::pump::cadence;
use crate::role_schedule::{self, LastSignal, ScheduleState};

use super::probe::ProjectSignals;

/// One autonomous wake: when it ran, how it ended, and what it saw.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeRecord {
    pub started_at: u64,
    #[serde(default)]
    pub ended_at: Option<u64>,
    /// `success` or `failed` once ended.
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub retry_at: Option<u64>,
    /// Fingerprints of the alarming projects this wake saw.
    #[serde(default)]
    pub seen: Vec<String>,
}

/// Why the mind is waking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WakeReason {
    /// Heartbeat due (or never run).
    Heartbeat { state: String },
    /// A project's signals changed into an alarming state.
    Signal { fingerprint: String },
}

/// The scheduler's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeDecision {
    Sleep(&'static str),
    Wake {
        reasons: Vec<WakeReason>,
        attempt: u32,
        seen: Vec<String>,
    },
}

/// Everything [`decide`] reads.
#[derive(Debug, Clone, Copy)]
pub struct WakeInputs<'a> {
    pub now: u64,
    pub heartbeat: &'a str,
    pub last: Option<&'a WakeRecord>,
    /// Start times of recent wakes, for the rolling-hour cap.
    pub recent: &'a [u64],
    pub max_wakes_per_hour: u32,
    pub paused: bool,
    pub signals: &'a [ProjectSignals],
}

/// Decide whether to wake now.
#[must_use]
pub fn decide(inputs: &WakeInputs<'_>) -> WakeDecision {
    if inputs.paused {
        return WakeDecision::Sleep("paused");
    }
    if let Some(last) = inputs.last {
        if last.ended_at.is_none() {
            return WakeDecision::Sleep("wake in flight");
        }
        if last.retry_at.is_some_and(|at| at > inputs.now) {
            return WakeDecision::Sleep("backing off after a failed wake");
        }
    }
    if wakes_in_last_hour(inputs.recent, inputs.now) >= inputs.max_wakes_per_hour as usize {
        return WakeDecision::Sleep("wake cap reached");
    }
    let seen = alarming(inputs.signals);
    let mut reasons = new_signals(&seen, inputs.last);
    if let Some(state) = heartbeat_due(inputs.heartbeat, inputs.last, inputs.now) {
        reasons.insert(0, WakeReason::Heartbeat { state });
    }
    if reasons.is_empty() {
        return WakeDecision::Sleep("nothing due");
    }
    WakeDecision::Wake {
        reasons,
        attempt: next_attempt(inputs.last),
        seen,
    }
}

/// Close a wake, setting its backoff on failure as cadence runs do.
pub fn finish(record: &mut WakeRecord, success: bool, now: u64) {
    record.ended_at = Some(now);
    if success {
        record.outcome = Some(String::from("success"));
        record.attempt = 0;
        record.retry_at = None;
    } else {
        record.outcome = Some(String::from("failed"));
        record.retry_at = Some(now.saturating_add(cadence::backoff_secs(record.attempt)));
    }
}

fn heartbeat_due(heartbeat: &str, last: Option<&WakeRecord>, now: u64) -> Option<String> {
    let signal = last.and_then(|record| {
        let at = record.ended_at?;
        Some(LastSignal {
            at: epoch(at),
            handle: String::from("mind-wake"),
            local_role: String::from("mind"),
        })
    });
    let report = role_schedule::evaluate(Some(heartbeat), signal.as_ref(), epoch(now));
    match report.state {
        ScheduleState::Never | ScheduleState::Due | ScheduleState::Overdue => {
            Some(role_schedule::state_label(report.state).to_string())
        }
        ScheduleState::None | ScheduleState::Ok => None,
    }
}

fn alarming(signals: &[ProjectSignals]) -> Vec<String> {
    signals
        .iter()
        .filter(|signal| signal.alarming())
        .map(ProjectSignals::fingerprint)
        .collect()
}

fn new_signals(seen: &[String], last: Option<&WakeRecord>) -> Vec<WakeReason> {
    seen.iter()
        .filter(|print| last.is_none_or(|record| !record.seen.contains(print)))
        .map(|print| WakeReason::Signal {
            fingerprint: print.clone(),
        })
        .collect()
}

fn next_attempt(last: Option<&WakeRecord>) -> u32 {
    last.map_or(1, |record| {
        if record.outcome.as_deref() == Some("success") {
            1
        } else {
            record.attempt.saturating_add(1).max(1)
        }
    })
}

fn wakes_in_last_hour(recent: &[u64], now: u64) -> usize {
    recent
        .iter()
        .filter(|at| now.saturating_sub(**at) < 3600)
        .count()
}

fn epoch(seconds: u64) -> DateTime<Utc> {
    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
    DateTime::from_timestamp(seconds, 0).unwrap_or_else(Utc::now)
}

#[cfg(test)]
#[path = "wake_test.rs"]
mod tests;
