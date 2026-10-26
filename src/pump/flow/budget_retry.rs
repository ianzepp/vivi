//! Shared budget-retry policy: a budget kill gets one retry at a raised
//! budget before the step counts as failed.
//!
//! A budget kill is machine evidence only — the pool's terminal timeout
//! status, or a finished job whose bounded pool summary reports agent exit
//! 124. Model prose, command output and receipt sections never count.

use crate::pool::dispatch::BUDGET_RANGE;
use crate::pump::attempt_record;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::jobs::{JobStatus, PoolJob};

/// The durable boolean payload key marking the raised-budget retry as spent;
/// missing means false.
pub(crate) const PAYLOAD_KEY: &str = "budget_retry";

/// The agent exit code the process runner uses for a timeout.
const TIMEOUT_EXIT: i32 = 124;

/// The legacy receipt boundary after which only the hand's own text follows.
const HAND_MESSAGE_BOUNDARY: &str = "hand's final message:";

/// Whether `role` may retry a budget kill at a raised budget: only the seven
/// thinking kinds — never merge, repairer, hand or head-ceo.
#[must_use]
pub(crate) fn eligible(role: RoleKind) -> bool {
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

/// The agent exit code in a receipt's bounded pool summary: only the lines
/// before the first section and before the hand's own text count.
fn summary_exit(receipt: &str) -> Option<i32> {
    let summary = receipt
        .split("\n\n--- ")
        .next()
        .and_then(|head| head.split(HAND_MESSAGE_BOUNDARY).next())
        .unwrap_or_default();
    attempt_record::agent_exit(summary)
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
