//! The list views of a health answer, each a pure read of the persisted rows.

use serde::Serialize;

use crate::pump::store::build_waits::{BuildPrerequisite, BuildWait, PrerequisiteState};

/// Exceptions listed at most, newest kept.
pub const MAX_EXCEPTIONS: usize = 50;
/// Longest exception text kept.
const MAX_REASON: usize = 300;

#[derive(Debug, Clone, Serialize)]
pub struct ExceptionView {
    pub at: u64,
    pub kind: String,
    pub unit: String,
    pub text: String,
}

/// One queued build wait: the dependent need, the open record that queued it
/// and the prerequisites that had not landed when it was observed. Every field
/// is the persisted projection verbatim, so nothing here judges a wait.
#[derive(Debug, Clone, Serialize)]
pub struct WaitingView {
    /// The dependent need that could not build.
    pub need: String,
    /// The handle of the open pump record for that need.
    pub record: String,
    /// The prerequisites that had not landed, in handle order.
    pub prerequisites: Vec<WaitingPrerequisite>,
    /// When the wait was observed, as a unix timestamp.
    pub observed_at: u64,
}

/// One prerequisite that kept a need from building, as it was classified.
#[derive(Debug, Clone, Serialize)]
pub struct WaitingPrerequisite {
    /// The prerequisite identity: the item handle.
    pub handle: String,
    /// The state that classified this prerequisite.
    pub state: PrerequisiteState,
    /// Why this state was read, when the classification carried a detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The outcome this prerequisite reported, when it reported one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// What moved after a cursor.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedSince {
    pub since: u64,
    pub exceptions: usize,
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

/// The persisted waits, in persisted (need-handle) order, with each wait's
/// prerequisites in handle order so two reads of one store agree.
#[must_use]
pub fn waiting(waits: &[BuildWait]) -> Vec<WaitingView> {
    waits
        .iter()
        .map(|wait| {
            let mut prerequisites: Vec<&BuildPrerequisite> = wait.prerequisites.iter().collect();
            prerequisites.sort_by(|left, right| left.handle.cmp(&right.handle));
            WaitingView {
                need: wait.need.clone(),
                record: wait.record.clone(),
                prerequisites: prerequisites
                    .into_iter()
                    .map(|prerequisite| WaitingPrerequisite {
                        handle: prerequisite.handle.clone(),
                        state: prerequisite.state,
                        detail: prerequisite.detail.clone(),
                        outcome: prerequisite.outcome.clone(),
                    })
                    .collect(),
                observed_at: wait.observed_at,
            }
        })
        .collect()
}

/// Exceptions strictly after `since`.
#[must_use]
pub fn changed_since(rows: &[(u64, String, String, String)], since: u64) -> ChangedSince {
    ChangedSince {
        since,
        exceptions: rows.iter().filter(|(at, ..)| *at > since).count(),
    }
}
