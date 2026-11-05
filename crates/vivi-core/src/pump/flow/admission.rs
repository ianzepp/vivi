//! Side-effect-free evaluation of a need's prerequisite readiness.

pub(crate) mod diagnostics;

use crate::pump::flow::mail::MailReader;
use crate::pump::flow::milestone::prereq::{Prereq, PrereqState, prerequisites};
use crate::pump::store::PumpStore;

/// The mechanical readiness classification for a need's prerequisites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdmissionState {
    /// No prerequisites remain unlanded.
    Ready,
    /// At least one prerequisite is open and none is held or closed unlanded.
    Waiting,
    /// At least one prerequisite is held or closed without landing.
    Blocked,
    /// The dependency graph or one of its prerequisite records could not be read.
    Unreadable(String),
}

/// An admission result with every non-landed prerequisite for diagnosis or projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Admission {
    pub state: AdmissionState,
    pub details: Vec<Prereq>,
}

/// Classifies `need` without raising exceptions or writing reporting state.
pub(crate) fn evaluate<M: MailReader>(store: &PumpStore, mail: &M, need: &str) -> Admission {
    let mut classified = match prerequisites(store, mail, need) {
        Ok(classified) => classified,
        Err(error) => {
            return Admission {
                state: AdmissionState::Unreadable(error.to_string()),
                details: Vec::new(),
            };
        }
    };
    classified.sort_by(|left, right| left.handle.cmp(&right.handle));
    classified.dedup_by(|left, right| left.handle == right.handle);

    let blocked = classified.iter().any(|prereq| {
        matches!(
            prereq.state,
            PrereqState::Held(_) | PrereqState::ClosedUnlanded(_)
        )
    });
    let waiting = classified
        .iter()
        .any(|prereq| prereq.state == PrereqState::Open);
    let details = classified
        .into_iter()
        .filter(|prereq| prereq.state != PrereqState::Landed)
        .collect();
    let state = if blocked {
        AdmissionState::Blocked
    } else if waiting {
        AdmissionState::Waiting
    } else {
        AdmissionState::Ready
    };
    Admission { state, details }
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;
