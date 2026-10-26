//! The shared bounded-retry decision and application for a transient step
//! failure: one entry the owning collectors drive a finished job through.
//!
//! The entry classifies the failure with the pure evidence rules of
//! [`classify`], persists the retry intent in the durable
//! `flow_retry_decisions` store, and only then enqueues the replacement: a
//! full-input copy ([`replay::copy_spec`]) of the failed job, held back from
//! dispatch until the fixed 30/60/120-second backoff of its retry ordinal
//! plus [`transient::jitter`]. The allowance is the store's
//! [`RETRY_LIMIT`](crate::pump::store::flow_retry::RETRY_LIMIT), keyed by
//! step coordinates and input generation and shared across failure classes
//! and correction executions, so alternating causes drain one budget and a
//! reopened store never resets a half-spent one.
//!
//! Every write is safe to repeat. The decision is the intent: it is
//! persisted first and a repeated ask returns it unchanged, so a collection
//! that crashes between the writes — or repeats after them — recovers the
//! enqueue, the identity write and the index row from the persisted decision
//! alone: the replacement id, its ordinal, its deadline and its input
//! generation all come from the decision or the failed job, never from the
//! later clock. The failed execution is never marked applied here; a
//! [`RetryOutcome::ReplacementPending`] answer tells the collector to keep
//! it unapplied until its replacement lands. Work of a withdrawn or
//! superseded run is never recreated, and no decision is recorded for it.

use crate::error::VivariumError;
use crate::pump::flow::index::{RunRow, StepJobRow, run_state};
use crate::pump::jobs::{self, JobSpec, JobStore, PoolJob, replay};
use crate::pump::store::PumpStore;
use crate::pump::store::flow_retry::{FlowRetryDecision, FlowRetryRequest};
use crate::pump::transient;

use super::classify;

/// The fixed dispatch backoff of a retry, in seconds, by ordinal: the first
/// replacement waits 30 seconds, the second 60, the third 120.
const BACKOFF_SECS: [u64; 3] = [30, 60, 120];

/// The step coordinates a retry budget is spent under: the same key the
/// durable store counts the [`RETRY_LIMIT`] allowance by.
pub(crate) struct RetryStep<'a> {
    pub(crate) need: &'a str,
    pub(crate) run: u32,
    pub(crate) node: &'a str,
    pub(crate) task: &'a str,
}

/// What the owning collector must do after the policy saw one failed job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetryOutcome {
    /// The failure asks for no bounded retry — the evidence classifies as
    /// none, or the run is withdrawn or superseded — so existing handling
    /// applies and the retry store is untouched.
    NotRetryable,
    /// The intent is persisted and the replacement job is enqueued: keep the
    /// failed execution unapplied until the replacement lands.
    ReplacementPending(FlowRetryDecision),
    /// The allowance is spent: apply the failure and keep this ordered
    /// decision history — every cause, ordinal and outcome — as its
    /// evidence.
    Exhausted { history: Vec<FlowRetryDecision> },
}

/// Decide and apply the bounded retry of one finished job's transient
/// failure.
///
/// `row` is the failed job's step-job row when its owner keeps one; owners
/// without step-jobs entries (a reslice, a slice merge) pass `None` and the
/// replacement is enqueued without an index row. A step-jobs owner's
/// replacement keeps the same attempt: the retry is a correction of the
/// execution, not a new one.
///
/// # Errors
/// Returns [`VivariumError`] when the run row, the retry store or the job
/// enqueue cannot be read or written.
pub(crate) fn retry(
    st: &mut impl JobStore,
    store: &PumpStore,
    failed: &PoolJob,
    step: &RetryStep<'_>,
    row: Option<&StepJobRow>,
    now: u64,
) -> Result<RetryOutcome, VivariumError> {
    let Some(class) = classify(failed) else {
        return Ok(RetryOutcome::NotRetryable);
    };
    if is_dead_run(&store.run_row(step.need, step.run)?) {
        return Ok(RetryOutcome::NotRetryable);
    }
    let generation = u32::try_from(replay::input_generation(&failed.payload)).unwrap_or(u32::MAX);
    let ordinal = provisional_ordinal(store, step, generation)?;
    let deadline = now
        + backoff(ordinal)
        + transient::jitter(&replacement_id(&failed.id, ordinal))
        + failed.budget;
    let request = FlowRetryRequest {
        need: step.need.to_owned(),
        run: step.run,
        node: step.node.to_owned(),
        task: step.task.to_owned(),
        generation,
        failed_job: failed.id.clone(),
        failure_class: class.to_owned(),
        replacement_job: None,
        deadline,
    };
    let decision = store.decide_flow_retry(&request, now)?;
    if !decision.allowed {
        return Ok(RetryOutcome::Exhausted {
            history: history(store, step, generation)?,
        });
    }
    Ok(RetryOutcome::ReplacementPending(apply_intent(
        st, store, failed, step, row, &decision,
    )?))
}

/// Finish an allowed `decision`: enqueue the replacement it names or mints,
/// record the replacement identity and the step-job row, and answer the
/// decision that names them. Each write is safe to repeat: the enqueue
/// no-ops on an identical job, the identity is written once, and the row is
/// written once.
///
/// # Errors
/// Returns [`VivariumError`] when any write fails.
fn apply_intent(
    st: &mut impl JobStore,
    store: &PumpStore,
    failed: &PoolJob,
    step: &RetryStep<'_>,
    row: Option<&StepJobRow>,
    decision: &FlowRetryDecision,
) -> Result<FlowRetryDecision, VivariumError> {
    let id = decision
        .replacement_job
        .clone()
        .unwrap_or_else(|| replacement_id(&failed.id, decision.ordinal));
    let not_before = decision.deadline.saturating_sub(failed.budget);
    jobs::enqueue(
        st,
        replacement_spec(failed, &id, not_before, row, step.need),
    )?;
    let decision = if decision.replacement_job.is_none() {
        store
            .set_flow_retry_replacement(&failed.id, &id)?
            .unwrap_or_else(|| decision.clone())
    } else {
        decision.clone()
    };
    if let Some(row) = row {
        let record = row.again(&id);
        if store.step_job(&id)?.is_none() {
            store.insert_step_job(step.need, &record)?;
        }
    }
    Ok(decision)
}

/// The full-input copy of `failed` named `id`: the replay metadata marks it
/// a replacement of `failed` held until `not_before`, and a step-jobs
/// owner's row restamps the same attempt. Everything else — routing,
/// checkout, publish rules, model, command, budget, and the payload with
/// its correction metadata and input generation — is the copy's
/// inheritance.
fn replacement_spec(
    failed: &PoolJob,
    id: &str,
    not_before: u64,
    row: Option<&StepJobRow>,
    need: &str,
) -> JobSpec {
    let mut spec = replay::copy_spec(failed, id);
    replay::stamp_replacement(&mut spec.payload, failed);
    replay::stamp_dispatch_not_before(&mut spec.payload, not_before);
    if let Some(row) = row {
        row.again(id).stamp(need, &mut spec.payload);
    }
    spec
}

/// The ordinal the next fresh decision of `step` should expect: one past the
/// allowance already spent, across every cause. The store re-counts
/// atomically and answers a repeat from its stored decision, so this
/// provisional count only sizes the fixed backoff and deadline of the
/// intent; the pump collects one job at a time and the two counts agree.
///
/// # Errors
/// Returns [`VivariumError`] when the history cannot be read.
fn provisional_ordinal(
    store: &PumpStore,
    step: &RetryStep<'_>,
    generation: u32,
) -> Result<u32, VivariumError> {
    let spent = history(store, step, generation)?
        .into_iter()
        .filter(|decision| decision.allowed)
        .count();
    Ok(u32::try_from(spent).unwrap_or(u32::MAX).saturating_add(1))
}

/// The ordered decision history of `step` and `generation`, oldest first.
///
/// # Errors
/// Returns [`VivariumError`] when the rows cannot be read.
fn history(
    store: &PumpStore,
    step: &RetryStep<'_>,
    generation: u32,
) -> Result<Vec<FlowRetryDecision>, VivariumError> {
    store.flow_retry_failures(step.need, step.run, step.node, step.task, generation)
}

/// The fixed dispatch backoff of `ordinal`, in seconds. An exhausted
/// ordinal keeps the last delay; its decision is never allowed.
fn backoff(ordinal: u32) -> u64 {
    let index = usize::try_from(ordinal.saturating_sub(1)).unwrap_or(usize::MAX);
    BACKOFF_SECS.get(index).copied().unwrap_or(BACKOFF_SECS[2])
}

/// The deterministic replacement identity of `failed_id` at retry
/// `ordinal`: the id of the retry chain the failed job belongs to, plus the
/// ordinal. Recovery from a persisted decision alone re-derives it.
fn replacement_id(failed_id: &str, ordinal: u32) -> String {
    format!("{}-r{ordinal}", retry_base(failed_id))
}

/// The id of the retry chain `job_id` belongs to: the id without a trailing
/// `-r<n>`, as the correction ids are with `-c<n>`.
fn retry_base(job_id: &str) -> &str {
    match job_id.rsplit_once("-r") {
        Some((base, tail)) if tail.parse::<u32>().is_ok() => base,
        _ => job_id,
    }
}

/// Whether a run in this state must not receive new work.
fn is_dead_run(run: &RunRow) -> bool {
    run.state == run_state::WITHDRAWN || run.state == run_state::SUPERSEDED
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod tests;
