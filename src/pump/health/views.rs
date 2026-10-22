//! The list views of a health answer, each a pure read of the state.

use serde::Serialize;

use crate::pump::board::PARKED;
use crate::pump::state::{State, Status, Unit};

/// Newest merges listed in [`Merges::recent`].
pub const RECENT_MERGES: usize = 10;
/// Exceptions listed at most, newest kept.
pub const MAX_EXCEPTIONS: usize = 50;
/// Unit changes listed at most in [`ChangedSince::units`], newest kept.
pub const MAX_CHANGES: usize = 200;
/// Longest reason text kept per blocked entry.
const MAX_REASON: usize = 300;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Merges {
    /// The pump's own landed-merge counter.
    pub total: u64,
    /// Newest first.
    pub recent: Vec<MergeView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeView {
    pub unit: String,
    pub need: String,
    pub title: String,
    pub commit: Option<String>,
    pub at: u64,
}

/// A unit parked off the dispatch path, or a need parked on a verdict.
#[derive(Debug, Clone, Serialize)]
pub struct BlockedView {
    /// `unit` or `need`.
    pub kind: &'static str,
    /// The unit id or need handle.
    pub id: String,
    pub need: String,
    /// The unit status, or the need's parked verdict.
    pub state: String,
    pub age_secs: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExceptionView {
    pub at: u64,
    pub kind: String,
    pub unit: String,
    pub text: String,
}

/// What moved after a cursor.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedSince {
    pub since: u64,
    /// Units whose current status began after `since`, newest first.
    pub units: Vec<UnitChange>,
    pub merges: usize,
    pub exceptions: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnitChange {
    pub unit: String,
    pub need: String,
    pub status: String,
    pub at: u64,
}

#[must_use]
pub fn merges(state: &State) -> Merges {
    let mut recent: Vec<MergeView> = state
        .units
        .iter()
        .filter_map(|(id, unit)| {
            unit.merged_at.map(|at| MergeView {
                unit: id.clone(),
                need: unit.need.clone(),
                title: unit.title.clone(),
                commit: unit.commit.clone(),
                at,
            })
        })
        .collect();
    recent.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.unit.cmp(&b.unit)));
    recent.truncate(RECENT_MERGES);
    Merges {
        total: state.merges,
        recent,
    }
}

/// Stuck, quarantined, needs-replan and held units, then needs parked on a
/// verdict; oldest first within each.
#[must_use]
pub fn blocked(state: &State, now: u64) -> Vec<BlockedView> {
    let mut units: Vec<BlockedView> = state
        .units
        .iter()
        .filter(|(_, unit)| parked_status(unit.status))
        .map(|(id, unit)| BlockedView {
            kind: "unit",
            id: id.clone(),
            need: unit.need.clone(),
            state: unit.status.as_str().to_owned(),
            age_secs: now.saturating_sub(unit.since),
            reason: clip(&unit_reason(unit)),
        })
        .collect();
    units.sort_by(|a, b| b.age_secs.cmp(&a.age_secs).then_with(|| a.id.cmp(&b.id)));
    let needs = state
        .needs
        .iter()
        .filter(|(_, record)| !record.withdrawn && PARKED.contains(&record.verdict.as_str()))
        .map(|(handle, record)| BlockedView {
            kind: "need",
            id: handle.clone(),
            need: handle.clone(),
            state: record.verdict.clone(),
            age_secs: 0,
            reason: clip(&record.reason),
        });
    units.extend(needs);
    units
}

fn parked_status(status: Status) -> bool {
    matches!(
        status,
        Status::Stuck | Status::Quarantined | Status::NeedsReplan | Status::Held
    )
}

/// Why a unit is parked: what it waits on, else its last attempt.
fn unit_reason(unit: &Unit) -> String {
    if let Some(on) = &unit.held_on {
        return format!("held on {on}");
    }
    if let Some(id) = &unit.quarantine {
        return format!("quarantine {id}");
    }
    unit.history.last().map_or_else(String::new, |attempt| {
        format!("{}: {}", attempt.why, attempt.detail)
    })
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_REASON {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_REASON).collect();
    out.push('…');
    out
}

/// Exceptions at or after `window`, newest last, at most [`MAX_EXCEPTIONS`].
#[must_use]
pub fn exceptions(rows: &[(u64, String, String, String)], window: u64) -> Vec<ExceptionView> {
    let mut out: Vec<ExceptionView> = rows
        .iter()
        .filter(|(at, ..)| *at >= window)
        .map(|(at, kind, unit, text)| ExceptionView {
            at: *at,
            kind: kind.clone(),
            unit: unit.clone(),
            text: clip(text),
        })
        .collect();
    if out.len() > MAX_EXCEPTIONS {
        out.drain(..out.len() - MAX_EXCEPTIONS);
    }
    out
}

/// Unit status changes, merges and exceptions strictly after `since`.
#[must_use]
pub fn changed_since(
    state: &State,
    rows: &[(u64, String, String, String)],
    since: u64,
) -> ChangedSince {
    let mut units: Vec<UnitChange> = state
        .units
        .iter()
        .filter(|(_, unit)| unit.since > since)
        .map(|(id, unit)| UnitChange {
            unit: id.clone(),
            need: unit.need.clone(),
            status: unit.status.as_str().to_owned(),
            at: unit.since,
        })
        .collect();
    units.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.unit.cmp(&b.unit)));
    units.truncate(MAX_CHANGES);
    ChangedSince {
        since,
        units,
        merges: state
            .units
            .values()
            .filter(|unit| unit.merged_at.is_some_and(|at| at > since))
            .count(),
        exceptions: rows.iter().filter(|(at, ..)| *at > since).count(),
    }
}
