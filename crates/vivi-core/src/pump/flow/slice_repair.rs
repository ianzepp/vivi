//! What a retried slice starts from. A failed attempt is repaired, not rebuilt
//! blind: the next attempt's brief carries the failure, and when the attempt
//! left a commit that only failed its check (a red formatter or task rung), the
//! next attempt starts from that commit and fixes it.
//!
//! A conflict starts again from the need-branch tip: the failed commit's base
//! has moved, so its work is rebuilt there, with the conflicting paths in hand.
//! A hand job that failed left no commit to keep.

use crate::pump::flow::verify_inputs::capped;
use crate::pump::jobs::{PoolJob, rung_result};

/// Most bytes of failure output a repair brief carries.
const FAILURE_CAP: usize = 4 * 1024;
/// The receipt mark git leaves on a conflicted merge.
const CONFLICT_MARK: &str = "CONFLICT";

/// How the next attempt repairs a failed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repair {
    /// The attempt that failed.
    pub attempt: u32,
    /// The failed attempt's commit to start from; none starts from the
    /// need-branch tip.
    pub from: Option<String>,
    /// What the attempt failed with.
    pub failure: String,
}

impl Repair {
    /// The repair a failed job (a slice merge or a hand job) gives the next
    /// attempt.
    #[must_use]
    pub(crate) fn of(failed: &PoolJob, attempt: u32) -> Self {
        let receipt = failed.outcome.as_ref().map_or("", |o| o.receipt.as_str());
        let conflict = receipt.contains(CONFLICT_MARK);
        let from = if conflict {
            None
        } else {
            failed
                .payload
                .get("hand_sha")
                .and_then(serde_json::Value::as_str)
                .filter(|sha| !sha.is_empty())
                .map(str::to_owned)
        };
        let detail = failed.outcome.as_ref().map_or("", |o| o.detail.as_str());
        let tail = if conflict {
            receipt
                .lines()
                .filter(|line| line.contains(CONFLICT_MARK))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            rung_result::parse(failed).tail
        };
        let failure = [detail.trim(), tail.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            attempt,
            from,
            failure: capped(&failure, FAILURE_CAP),
        }
    }

    /// The brief section that tells the hand where it starts and what failed.
    #[must_use]
    pub(crate) fn section(&self) -> String {
        let start = match &self.from {
            Some(sha) => format!(
                "This checkout starts from that attempt's commit {sha}: its work is here. \
                 Fix the failure below; do not start the work over."
            ),
            None => String::from(
                "Its work could not be kept (it conflicted with the need branch, or left \
                 no commit), so this checkout starts from the need-branch tip. Do the \
                 work again with the failure below in mind.",
            ),
        };
        let failure = if self.failure.is_empty() {
            "(no output was recorded)"
        } else {
            self.failure.as_str()
        };
        format!(
            "EARLIER ATTEMPT:\nAttempt {} of this slice failed. {start}\n\nFailure:\n{failure}",
            self.attempt
        )
    }
}

#[cfg(test)]
#[path = "slice_repair_test.rs"]
mod tests;
