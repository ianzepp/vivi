//! Reads a finished slice merge job from machine facts.
//!
//! The job is model-free, so the receipt is the only evidence: the merge or
//! rung command's exit, the `CONFLICT` report from git, and the runner's
//! `runner publish:` line. A result that the receipt cannot settle is
//! `Unclear`, and git decides later.

use std::collections::BTreeSet;

use crate::pump::jobs::{JobStatus, PoolJob, rung_result};

/// What a finished slice merge job turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SliceMergeResult {
    /// The runner published the need branch; carries the remote sha.
    Merged(String),
    /// The merge hit conflicts; carries the sorted, deduplicated paths.
    Conflict(Vec<String>),
    /// The formatter or the task rung failed; carries the failure tail.
    Red(String),
    /// The receipt does not settle it (timeout, lost, no publish line).
    Unclear,
}

const CONFLICT_MARK: &str = "Merge conflict in ";

/// Read a slice merge job. None while it has not reached a terminal pool
/// state.
#[must_use]
pub(crate) fn classify(job: &PoolJob) -> Option<SliceMergeResult> {
    match job.status {
        JobStatus::Pending | JobStatus::Dispatched => None,
        JobStatus::Timeout | JobStatus::Lost => Some(SliceMergeResult::Unclear),
        JobStatus::Finished => Some(finished(job)),
    }
}

fn finished(job: &PoolJob) -> SliceMergeResult {
    let Some(outcome) = &job.outcome else {
        return SliceMergeResult::Unclear;
    };
    if matches!(outcome.state.as_str(), "timeout" | "lost") {
        return SliceMergeResult::Unclear;
    }
    if outcome.agent_success() == Some(false) {
        return if outcome.receipt.contains("CONFLICT") {
            SliceMergeResult::Conflict(conflict_paths(&outcome.receipt))
        } else {
            SliceMergeResult::Red(rung_result::parse(job).tail)
        };
    }
    match outcome.runner_publish() {
        Some(publish) if publish.succeeded() => SliceMergeResult::Merged(publish.remote_sha),
        _ => SliceMergeResult::Unclear,
    }
}

fn conflict_paths(receipt: &str) -> Vec<String> {
    let paths: BTreeSet<String> = receipt
        .lines()
        .filter_map(|line| line.split_once(CONFLICT_MARK))
        .map(|(_, path)| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .collect();
    paths.into_iter().collect()
}

#[cfg(test)]
#[path = "classify_test.rs"]
mod tests;
