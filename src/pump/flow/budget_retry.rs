//! Shared budget-retry policy: a budget kill gets one retry at a raised
//! budget before the step counts as failed.
//!
//! A budget kill is machine evidence only — the pool's terminal timeout
//! status, or a finished job whose bounded pool summary reports agent exit
//! 124. Model prose, command output and receipt sections never count. A
//! killed thinking-role job gets one retry at [`retry_budget`] before the
//! step counts as failed.

use crate::pool::dispatch::BUDGET_RANGE;
use crate::pool::record;
use crate::pump::attempt_record;
use crate::pump::envelope::correct;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::jobs::{JobSpec, JobStatus, PoolJob};

/// The durable boolean payload key marking the raised-budget retry as spent;
/// missing means false.
pub(crate) const PAYLOAD_KEY: &str = "budget_retry";

/// The agent exit code the process runner uses for a timeout.
const TIMEOUT_EXIT: i32 = 124;

/// The legacy receipt boundary after which only the hand's own text follows.
const HAND_MESSAGE_BOUNDARY: &str = "hand's final message:";

/// Whether `role` thinks: one of Researcher, Planner, Admitter, Slicer,
/// Auditor, Judge or Cto — and so may retry a budget kill at a raised
/// budget. Never merge, repairer, hand or head-ceo.
#[must_use]
pub(crate) fn is_thinking(role: RoleKind) -> bool {
    matches!(
        role,
        RoleKind::Researcher
            | RoleKind::Planner
            | RoleKind::Admitter
            | RoleKind::Slicer
            | RoleKind::Auditor
            | RoleKind::Judge
            | RoleKind::Cto
    )
}

/// Whether the pool evidence says `job` was killed by its time budget: a
/// terminal timeout status, or a finished job whose bounded pool summary
/// reports agent exit 124. Pending, dispatched and lost jobs never count,
/// and neither does exit-124 text in the hand's message, stderr or runner
/// log.
#[must_use]
pub(crate) fn is_budget_kill(job: &PoolJob) -> bool {
    match job.status {
        JobStatus::Timeout => true,
        JobStatus::Finished => job
            .outcome
            .as_ref()
            .is_some_and(|outcome| summary_exit(&outcome.receipt) == Some(TIMEOUT_EXIT)),
        JobStatus::Pending | JobStatus::Dispatched | JobStatus::Lost => false,
    }
}

/// The agent exit code in a receipt's bounded pool summary: only the text
/// before the first pool-rendered section heading and before the hand's own
/// text counts. A complete facts/receipt/stderr-tail/runner-log-tail heading
/// bounds the summary, including single-newline fixtures; unknown headings
/// never do, so quoted exit codes under them are never timeout evidence.
fn summary_exit(receipt: &str) -> Option<i32> {
    let (head, _) = record::split_sections(receipt);
    let summary = head
        .lines()
        .take_while(|line| !line.starts_with(HAND_MESSAGE_BOUNDARY))
        .collect::<Vec<_>>()
        .join("\n");
    attempt_record::agent_exit(&summary)
}

/// Copy every execution input and the payload of `job` into a fresh
/// [`JobSpec`] named `new_id`: task, routing restrictions, checkout,
/// publish and delete refs, extra targets, model, command, built-in
/// charter, units, source jobs and unrelated payload all survive. Runtime
/// ownership and result fields — pool, timestamps, status, outcome,
/// applied-at, refusal counters and held state — have no spec identity and
/// are left behind.
#[must_use]
pub(crate) fn rerun_spec(job: &PoolJob, new_id: &str) -> JobSpec {
    job.rerun_spec(new_id)
}

/// Whether `job`'s durable payload marks its raised-budget retry as spent.
#[must_use]
pub(crate) fn spent(job: &PoolJob) -> bool {
    job.payload
        .get(PAYLOAD_KEY)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The budget to retry with after a budget kill: the saved budget doubled,
/// capped at the pool's maximum. `None` unless the saved budget is usable by
/// the pool and the raise is strict — a budget at or above the ceiling
/// cannot rise again. Reads no configuration.
#[must_use]
pub(crate) fn raised_budget(saved_budget: u64) -> Option<u64> {
    let raised = saved_budget.saturating_mul(2).min(*BUDGET_RANGE.end());
    (BUDGET_RANGE.contains(&saved_budget) && raised > saved_budget).then_some(raised)
}

/// The raised budget a budget-killed `job` may retry with, or `None` when
/// the retry is refused: correction (`envelope-fix`) jobs have their own
/// budget policy, a job whose [`PAYLOAD_KEY`] marker is set has already
/// spent its one retry, and [`raised_budget`] refuses budgets it cannot
/// strictly raise. Reads no configuration; callers gate eligibility with
/// the typed role kind.
#[must_use]
pub(crate) fn retry_budget(job: &PoolJob) -> Option<u64> {
    if job.kind == correct::KIND || spent(job) {
        return None;
    }
    raised_budget(job.budget)
}

/// Mark `payload` so [`spent`] reports the raised-budget retry as spent. A
/// null payload becomes an object; a payload of any other shape is left
/// untouched.
pub(crate) fn mark_retry(payload: &mut serde_json::Value) {
    match payload {
        serde_json::Value::Object(map) => {
            map.insert(PAYLOAD_KEY.to_owned(), serde_json::Value::Bool(true));
        }
        serde_json::Value::Null => {
            let mut map = serde_json::Map::new();
            map.insert(PAYLOAD_KEY.to_owned(), serde_json::Value::Bool(true));
            *payload = serde_json::Value::Object(map);
        }
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Array(_) => {}
    }
}

#[cfg(test)]
#[path = "budget_retry_test.rs"]
mod tests;

#[cfg(test)]
#[path = "budget_retry_flow_test.rs"]
mod budget_retry_flow_test;
