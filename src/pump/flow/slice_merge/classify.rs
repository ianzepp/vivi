//! Reads a finished slice merge job from machine facts.
//!
//! The job is model-free, so the receipt is the only evidence: the merge or
//! rung command's exit, the `CONFLICT` report from git, and the runner's
//! `runner publish:` line. A result that the receipt cannot settle is
//! `Unclear`, and git decides later.

use std::collections::BTreeSet;

use crate::pump::jobs::rung_result::{self, EnvFailure};
use crate::pump::jobs::{JobStatus, PoolJob};

/// What a finished slice merge job turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SliceMergeResult {
    /// The runner published the need branch; carries the remote sha.
    Merged(String),
    /// The merge hit conflicts; carries the sorted, deduplicated paths.
    Conflict(Vec<String>),
    /// The formatter or the task rung failed; carries the failure tail.
    Red(String),
    /// The worker lacked a tool or its host setup failed: the job says
    /// nothing about the code, so the pump holds instead of retrying.
    Env(EnvFailure),
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
        JobStatus::Timeout | JobStatus::Lost => Some(unsettled(job)),
        JobStatus::Finished => Some(finished(job)),
    }
}

fn finished(job: &PoolJob) -> SliceMergeResult {
    let Some(outcome) = &job.outcome else {
        return SliceMergeResult::Unclear;
    };
    if matches!(outcome.state.as_str(), "timeout" | "lost") {
        return unsettled(job);
    }
    if outcome.agent_success() == Some(false) {
        if outcome.receipt.contains("CONFLICT") {
            return SliceMergeResult::Conflict(conflict_paths(&outcome.receipt));
        }
        return rung_result::env_failure(job).map_or_else(
            || SliceMergeResult::Red(rung_result::parse(job).tail),
            SliceMergeResult::Env,
        );
    }
    match outcome.runner_publish() {
        Some(publish) if publish.succeeded() => SliceMergeResult::Merged(publish.remote_sha),
        _ => unsettled(job),
    }
}

/// A job the receipt cannot settle: an environment failure when its facts
/// show one (a setup failure carries an outcome), otherwise unclear.
fn unsettled(job: &PoolJob) -> SliceMergeResult {
    rung_result::env_failure(job).map_or(SliceMergeResult::Unclear, SliceMergeResult::Env)
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
