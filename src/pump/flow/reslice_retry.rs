//! The raised-budget retry of a budget-killed reslice job.
//!
//! The ruling: a budget kill gets one retry at a raised budget before the
//! step counts as failed. The retry re-runs the same request — the same
//! reslice task and payload — at twice the failed budget, capped by the
//! pool's budget range, and is durably marked so it never retries again.

use crate::error::VivariumError;
use crate::pump::flow::budget_retry;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobStore, PoolJob};

/// Answer whether the budget-killed reslice `job` raised its one retry.
/// A job the pool evidence does not call a budget kill
/// ([`budget_retry::is_budget_kill`]) is never retried and answers `false`.
///
/// With a legal raise, a deterministic full-input copy of `job`
/// ([`budget_retry::rerun_spec`]) named one attempt on is enqueued on the
/// same reslice task at [`budget_retry::retry_budget`], marked durably as
/// the spent retry ([`budget_retry::mark_retry`]), and only then is the
/// failed original marked applied; the reslice row and its slice stay
/// untouched, and no new task is sent. A copy already queued is not queued
/// twice. Without a legal raise — the retry spent, or a saved budget the
/// pool cannot strictly raise — the answer is `false` and the caller fails
/// the reslice through [`super::RESLICE_FAILED`].
///
/// # Errors
/// Returns a [`VivariumError`] when the copy cannot be enqueued.
pub(super) fn raise<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    job: &PoolJob,
) -> Result<bool, VivariumError> {
    if !budget_retry::is_budget_kill(job) {
        return Ok(false);
    }
    let Some(budget) = budget_retry::retry_budget(job) else {
        return Ok(false);
    };
    let Some(id) = next_attempt(&job.id) else {
        return Ok(false);
    };
    let mut spec = budget_retry::rerun_spec(job, &id);
    spec.budget = budget;
    budget_retry::mark_retry(&mut spec.payload);
    if !st.jobs().contains_key(&id) {
        jobs::enqueue(st, spec)?;
    }
    jobs::mark_applied(st, &job.id, ctx.now());
    Ok(true)
}

/// The id one attempt after `id`'s trailing `-a<n>`: `x-a2` from `x-a1`.
/// `None` when the id names no attempt number or that number is `u32::MAX`.
fn next_attempt(id: &str) -> Option<String> {
    let (base, n) = id.rsplit_once("-a")?;
    let Ok(n) = n.parse::<u32>() else {
        return None;
    };
    Some(format!("{base}-a{}", n.checked_add(1)?))
}
