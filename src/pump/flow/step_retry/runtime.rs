//! Collector-facing runtime over the durable retry stores: schedule one
//! same-step redispatch, then materialize it when its deadline arrives.
//!
//! [`schedule`] turns the classifier's transient evidence into a typed
//! [`Decision`] without touching the failed job's application state. The
//! immutable intent is reserved first —
//! [`crate::pump::store::step_retry`] owns the per-generation budget — and
//! only a successful reservation produces the distinct replacement
//! execution ([`crate::pump::store::step_execution`] supersedes the failed
//! generation). The recorded inputs are complete: the failed job's full
//! spec under the chained replacement identity, the step's run, node and
//! task with the advanced attempt stamped into the payload, and the chosen
//! deadline as both the stored intent and the job's dispatch hold.
//!
//! [`materialize`] replays a stored intent — enqueue, step-job index,
//! execution activation, step state, progress stamps — and rebuilds no
//! input: the recorded spec, identity and deadline are the only truth, so
//! every repeated pass is a no-op. Exhaustion carries the generation's
//! ordered history and stamps the durable [`EXHAUSTED_KEY`] payload marker;
//! it grants no fallback retry or correction budget. Owners keep control of
//! failure routes and publication settlement: [`settle`] only records that
//! a replacement's result was applied.

use crate::error::VivariumError;
use crate::pump::flow::index::{StepJobRow, StepState};
use crate::pump::jobs::{self, JobSpec, JobStore, PoolJob};
use crate::pump::store::PumpStore;
use crate::pump::store::step_execution::ExecutionKey;
use crate::pump::store::step_retry::{RetryIntent, RetryIntentRow, RetryReservation};
use crate::pump::transient;

use super::Transient;

/// The durable payload key marking the job whose step spent its retry
/// budget. The value is the exhausted failure class, so any later pass can
/// refuse fallback retry and correction budgets for the step.
pub const EXHAUSTED_KEY: &str = "flow-step-retry-exhausted";

/// Seconds before the first redispatch of one input generation.
const FIRST_DELAY: u64 = 30;

/// Seconds before the second redispatch of one input generation.
const SECOND_DELAY: u64 = 120;

/// One finished step job offered to [`schedule`]: the classifier's evidence
/// plus every identity the replacement must preserve. Cheap to assemble —
/// it owns clones, never borrows the caller's state.
#[derive(Debug, Clone)]
pub struct FailedStep {
    /// The failed input's execution key.
    pub key: ExecutionKey,
    /// The input generation the failed execution belongs to.
    pub generation: u32,
    /// The identity of the failed execution.
    pub execution: String,
    /// The failed durable job: the source of the complete replacement copy.
    pub job: PoolJob,
    /// The failed job's step index row: run, node, task and attempt.
    pub row: StepJobRow,
    /// The classifier's transient evidence; `None` leaves the step alone.
    pub transient: Option<Transient>,
}

/// What one [`schedule`] pass decided for a finished step job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// No retryable transient: the owner's ordinary failure route and
    /// publication settlement keep control. No store row is written.
    NoTransient,
    /// A replacement is durably reserved: materialize it at or after the
    /// recorded deadline.
    Pending(Box<PendingRetry>),
    /// The input generation spent its budget: no same-step redispatch
    /// remains, and no fallback retry or correction budget is granted.
    Exhausted(Box<ExhaustedRetry>),
}

/// One durably reserved replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRetry {
    /// The stored intent: the recorded replacement identity, deadline and
    /// full replacement inputs, answered unchanged on every replay.
    pub intent: RetryIntentRow,
    /// The distinct execution identity the replacement activates on.
    pub execution: String,
}

/// A spent generation: the ordered decision history a collector replays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExhaustedRetry {
    /// The failure class that spent the last budget slot.
    pub class: String,
    /// The generation's stored intents, oldest ordinal first.
    pub history: Vec<RetryIntentRow>,
}

/// Schedule the same-step redispatch a finished step job's transient
/// evidence asks for, reserving the durable intent before any application
/// state moves.
///
/// Without transient evidence the step is left untouched
/// ([`Decision::NoTransient`]). With evidence, the replacement identity
/// chains from the failed job's base (`step-x` → `step-x-r1` → `step-x-r2`),
/// the deadline is `now` plus the fixed delay for the redispatch's position
/// — 30 and then 120 seconds — plus [`transient::jitter`]'s deterministic
/// 0-14 second spread, and the store answers replays from the stored
/// intent, so repeated scheduling reserves exactly one replacement.
///
/// # Errors
/// Returns [`VivariumError`] when the stores cannot be read or written.
pub fn schedule(
    st: &mut impl JobStore,
    store: &PumpStore,
    failed: FailedStep,
    now: u64,
) -> Result<Decision, VivariumError> {
    let Some(seen) = &failed.transient else {
        return Ok(Decision::NoTransient);
    };
    let replacement = replacement_id(&failed.job.id);
    let deadline = now
        .saturating_add(redispatch_delay(&replacement))
        .saturating_add(transient::jitter(&replacement));
    match reserve_intent(store, &failed, seen, &replacement, deadline)? {
        RetryReservation::Exhausted(history) => {
            mark_exhausted(st, &failed.job.id, seen.class);
            Ok(Decision::Exhausted(Box::new(ExhaustedRetry {
                class: seen.class.to_owned(),
                history,
            })))
        }
        RetryReservation::Reserved(intent) => pending(store, intent),
        RetryReservation::Existing(intent) => pending(store, intent),
    }
}

/// Reserve the immutable intent, then produce the pending decision: the
/// recorded intent plus the distinct execution it activates on.
fn pending(store: &PumpStore, intent: RetryIntentRow) -> Result<Decision, VivariumError> {
    let execution = ensure_execution(store, &intent)?;
    Ok(Decision::Pending(Box::new(PendingRetry {
        intent,
        execution,
    })))
}

/// Build the intent's complete replacement inputs and reserve it.
fn reserve_intent(
    store: &PumpStore,
    failed: &FailedStep,
    seen: &Transient,
    replacement: &str,
    deadline: u64,
) -> Result<RetryReservation, VivariumError> {
    store.reserve_retry(&RetryIntent {
        key: failed.key.clone(),
        generation: failed.generation,
        execution: failed.execution.clone(),
        failed_job: failed.job.id.clone(),
        failure_class: seen.class.to_owned(),
        evidence: seen.evidence.clone(),
        replacement_job: replacement.to_owned(),
        spec: replacement_spec(failed, replacement, deadline),
        deadline,
    })
}

/// The complete recorded replacement inputs: every execution field of the
/// failed job under the replacement identity, the step's run, node and task
/// with the advanced attempt stamped into the payload, and the dispatch
/// hold at the deadline.
fn replacement_spec(failed: &FailedStep, replacement: &str, deadline: u64) -> JobSpec {
    let mut spec = failed.job.rerun_spec(replacement);
    failed
        .row
        .next(replacement)
        .stamp(&failed.key.need, &mut spec.payload);
    spec.not_before = deadline;
    spec
}

/// The replacement identity of a retry chain: the failed job's retry base
/// plus the next `-r<n>` ordinal, so `step-x` chains `step-x-r1`, then
/// `step-x-r2`, like corrections chain `-c<n>`.
#[must_use]
pub fn replacement_id(failed_job: &str) -> String {
    let (base, n) = retry_ordinal(failed_job);
    format!("{base}-r{}", n + 1)
}

/// How many `-r<n>` redispatches lie behind a job id, and the base they
/// chain from: `0` and the id itself for an unchained job.
fn retry_ordinal(job_id: &str) -> (&str, u32) {
    if let Some((base, tail)) = job_id.rsplit_once("-r")
        && let Ok(n) = tail.parse::<u32>()
    {
        return (base, n);
    }
    (job_id, 0)
}

/// The fixed delay for a replacement's position in its chain: 30 seconds
/// for the first redispatch, 120 for every later one.
fn redispatch_delay(replacement: &str) -> u64 {
    match retry_ordinal(replacement).1 {
        0 | 1 => FIRST_DELAY,
        _ => SECOND_DELAY,
    }
}

/// The execution identity the replacement activates on, minting the next
/// generation when no replay or earlier pass already did: an execution the
/// replacement already activates is reused, a live failed execution is
/// consumed, and a consumed one has its next generation read back.
fn ensure_execution(store: &PumpStore, intent: &RetryIntentRow) -> Result<String, VivariumError> {
    if let Some(active) = store.execution_by_job(&intent.replacement_job)? {
        return Ok(active.execution);
    }
    let failed = store.execution(&intent.execution)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "retry {}: failed execution {} is unknown",
            intent.replacement_job, intent.execution
        ))
    })?;
    if !failed.superseded {
        return Ok(store.consume_retry(&intent.execution)?.execution);
    }
    Ok(execution_identity(&failed.key(), failed.generation + 1))
}

/// The deterministic execution identity of one generation of `key`, the
/// same shape the execution store mints.
fn execution_identity(key: &ExecutionKey, generation: u32) -> String {
    format!(
        "{}:r{}:{}:{}:g{}",
        key.need, key.run, key.task, key.phase, generation
    )
}

/// Materialize a reserved replacement without rebuilding any input.
///
/// The recorded spec is enqueued verbatim (a stable identity makes repeated
/// passes a no-op and keeps the dispatch hold at the recorded deadline),
/// the step-job index gains the replacement row only when it lacks one,
/// the recorded execution activates the replacement at its advanced
/// attempt, the step runs again, and the intent records its
/// materialization — first stamp wins.
///
/// Call it at or after [`PendingRetry::intent`]'s deadline: before it, the
/// enqueued job simply stays pending. On failure nothing is half-applied
/// that a repeated call cannot finish.
///
/// # Errors
/// Returns [`VivariumError`] when the stores cannot be read or written.
pub fn materialize(
    st: &mut impl JobStore,
    store: &PumpStore,
    pending: &PendingRetry,
    at: u64,
) -> Result<(), VivariumError> {
    let intent = &pending.intent;
    let execution = ensure_execution(store, intent)?;
    let row = indexed_row(intent)?;
    jobs::enqueue(st, intent.spec.clone())?;
    if store.step_job(&row.job)?.is_none() {
        store.insert_step_job(&intent.need, &row)?;
    }
    store.activate_job(&execution, &row.job, row.attempt)?;
    store.mark_retry_materialized(&row.job, at)?;
    store.set_step_state(&row.task, StepState::Running, None)
}

/// The replacement's step index row, read back from the recorded payload:
/// the run and task are the intent's, the node and advanced attempt travel
/// inside the spec.
fn indexed_row(intent: &RetryIntentRow) -> Result<StepJobRow, VivariumError> {
    Ok(StepJobRow {
        job: intent.replacement_job.clone(),
        run: intent.run,
        node: payload_str(intent, "node")?,
        task: intent.task.clone(),
        attempt: payload_attempt(intent)?,
    })
}

/// One recorded string payload field.
fn payload_str(intent: &RetryIntentRow, key: &str) -> Result<String, VivariumError> {
    intent
        .spec
        .payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| missing_payload(intent, key))
}

/// The recorded advanced attempt.
fn payload_attempt(intent: &RetryIntentRow) -> Result<u32, VivariumError> {
    let raw = intent
        .spec
        .payload
        .get("attempt")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| missing_payload(intent, "attempt"))?;
    u32::try_from(raw).map_err(|_| missing_payload(intent, "attempt"))
}

fn missing_payload(intent: &RetryIntentRow, key: &str) -> VivariumError {
    VivariumError::Other(format!(
        "retry intent {}: the recorded spec payload has no {key}",
        intent.replacement_job
    ))
}

/// Record that a materialized replacement's result was applied:
/// settlement progress, idempotent, first stamp wins.
///
/// # Errors
/// Returns [`VivariumError`] when the intent is unknown or the row cannot
/// be written.
pub fn settle(store: &PumpStore, replacement_job: &str, at: u64) -> Result<(), VivariumError> {
    store.settle_retry_intent(replacement_job, at)
}

/// The failure class a job's step exhausted its retry budget with, from the
/// durable [`EXHAUSTED_KEY`] payload marker.
#[must_use]
pub fn exhausted_class(job: &PoolJob) -> Option<&str> {
    job.payload
        .get(EXHAUSTED_KEY)
        .and_then(serde_json::Value::as_str)
}

/// Stamp the exhausted class onto the failed job's durable payload, so no
/// later pass grants the step a fallback retry or correction budget.
fn mark_exhausted(st: &mut impl JobStore, job_id: &str, class: &str) {
    if let Some(fields) = st
        .jobs_mut()
        .get_mut(job_id)
        .and_then(|job| job.payload.as_object_mut())
    {
        fields.insert(
            EXHAUSTED_KEY.to_owned(),
            serde_json::Value::String(class.to_owned()),
        );
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
