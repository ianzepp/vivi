//! Durable allocation and replay of bounded transient replacements.
//!
//! The seam between the classification vocabulary of [`super`] and the
//! frozen `recovery_intents` rows of the pump store: a finished job whose
//! machine evidence classifies a transient failure and passes the
//! publication-safety gate gets one of at most three redispatches per step
//! input, frozen as an intent before any enqueue, index or application
//! effect fires. Collectors hand a finished job to [`allocate`] and act on
//! the [`Decision`]; the crash seam is [`restore`].
//!
//! The allowance is per step input, never per job or attempt. The payload's
//! recovery metadata names the input and the ordinal already spent, and
//! every policy that copies a payload — autonomous reruns (`-a<n>`),
//! envelope corrections (`-c<n>`, via the correction chain's provenance),
//! budget retries (their own marker) — inherits it unchanged, so none of
//! them can replenish the allowance. The replacement is a full-input copy of
//! the failed job built by [`PoolJob::rerun_spec`], renamed to the step
//! input's next `-tr` ordinal and re-stamped with recovery metadata; nothing
//! else about the request changes.
//!
//! Every write is idempotent by construction. Freezing a failed job that
//! already has an intent returns the stored row, so a repeated collection
//! never spends another ordinal or moves the persisted deadline; recording
//! the replacement skips a job that is already present. [`restore`] is the
//! crash seam: an intent whose replacement is missing — an earlier process
//! marked the enqueue complete but the state save never landed — is
//! restored from the frozen request after the live flow state is validated.
//!
//! Staged like [`super::safety`]: the collector that wires these calls into
//! collection lands in a later slice.
#![cfg_attr(not(test), allow(dead_code))]

use crate::error::VivariumError;
use crate::pump::flow::index::{StepJobRow, StepState};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobStore, PoolJob};
use crate::pump::store::PumpStore;
use crate::pump::store::recovery::{RecoveryIntentRow, recovery_state};
use crate::pump::transient;

use super::safety::{self, RecoverySafety};
use super::{RecoveryMeta, base_id, metadata, retries_spent, stamp_metadata, transient_class};

/// The three redispatch delays of a step input, in seconds: the first
/// retry lands a minute out, the second two minutes, the third four.
const DELAYS: [u64; 3] = [60, 120, 240];

/// How many transient redispatches one step input may spend.
pub(crate) const MAX_RETRIES: u32 = 3;

/// What a collector adapter should do with a finished job after [`allocate`]
/// looked at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// The failure is not a transient one this module recovers: ordinary
    /// collection owns the job.
    NotEligible,
    /// The intent was frozen now and the replacement was recorded.
    Queued {
        /// The replacement job's id.
        replacement: String,
        /// The absolute deadline persisted with the intent.
        deadline: u64,
    },
    /// The failed job already had an intent, and its stored answer was
    /// served: the persisted deadline stands, the recorded replacement was
    /// re-asserted present, and no ordinal was spent.
    AlreadyRecorded {
        /// The replacement job's id.
        replacement: String,
        /// The deadline persisted with the first freeze.
        deadline: u64,
    },
    /// The step input has spent all [`MAX_RETRIES`] redispatches.
    Exhausted,
    /// The safety gate refused: the work already landed, or git could not
    /// settle the remote. `code` is a [`super::safety::code`] word.
    Unsafe {
        /// Why the gate refused; a [`super::safety::code`] word.
        code: &'static str,
    },
}

/// The answer of [`restore`] for one pending or applied intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Restore {
    /// The missing replacement was re-enqueued from the frozen request.
    Restored,
    /// The replacement is already recorded; nothing was written.
    Present,
    /// The live flow state no longer matches the intent, so nothing was
    /// written; `reason` says what moved.
    Stale {
        /// What no longer matches.
        reason: String,
    },
}

/// Offer a collected failed `job` to the transient-retry allowance.
///
/// `binding` is the job's indexed step-job row for an indexed step job, and
/// `None` for an independently collected owned job; the binding travels onto
/// the intent untouched, so each collector keeps its own collection
/// ownership. The decision tells the caller who owns the step's outcome.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read or written or
/// the replacement cannot be enqueued.
pub(crate) fn allocate<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    job: &PoolJob,
    binding: Option<&StepJobRow>,
) -> Result<Decision, VivariumError> {
    let Some(class) = transient_class(job) else {
        return Ok(Decision::NotEligible);
    };
    if let RecoverySafety::Published { code } | RecoverySafety::Unclear { code } =
        safety::decide(ctx, job, class)
    {
        return Ok(Decision::Unsafe { code });
    }
    // A repeated collection of an already-frozen job is answered from the
    // stored intent: same deadline, same replacement, no ordinal spent.
    if let Some(existing) = store.recovery_intent_of_failed_job(&job.id)? {
        record(ctx, st, store, &existing)?;
        return Ok(recorded(&existing));
    }
    let task = task_of(job, binding);
    let input = step_input(job);
    let ordinal = spent(store, job, &task, &input)? + 1;
    if ordinal > MAX_RETRIES {
        return Ok(Decision::Exhausted);
    }
    let row = intent_row(ctx, job, binding, class, &task, &input, ordinal);
    let stored = store.insert_recovery_intent(&row)?;
    record(ctx, st, store, &stored)?;
    Ok(Decision::Queued {
        replacement: stored.replacement_job.clone(),
        deadline: stored.deadline,
    })
}

/// Replay one frozen intent whose replacement may be missing: an earlier
/// process froze the intent and marked its enqueue complete, but crashed
/// before the state save carried the replacement job. The frozen request is
/// re-enqueued as it stands — the persisted deadline is never reset and a
/// present replacement is never duplicated — after the live flow state is
/// validated against the intent.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read or written or
/// the replacement cannot be enqueued.
pub(crate) fn restore<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    intent: &RecoveryIntentRow,
) -> Result<Restore, VivariumError> {
    if let Some(reason) = stale_reason(store, intent)? {
        return Ok(Restore::Stale { reason });
    }
    if record(ctx, st, store, intent)? {
        Ok(Restore::Restored)
    } else {
        Ok(Restore::Present)
    }
}

/// Record the replacement of `intent`: enqueue the frozen request unless the
/// job id is already present, index the frozen step-job binding unless its
/// row is already written, and mark the intent applied (idempotent — the
/// first stamp stands). Answers whether anything was written.
fn record<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    intent: &RecoveryIntentRow,
) -> Result<bool, VivariumError> {
    let mut wrote = false;
    if !st.jobs().contains_key(&intent.replacement_job) {
        jobs::enqueue(st, intent.request.clone())?;
        wrote = true;
    }
    if let Some(bind) = &intent.step_job
        && store.step_job(&intent.replacement_job)?.is_none()
    {
        store.insert_step_job(&intent.need, bind)?;
        wrote = true;
    }
    store.mark_recovery_applied(&intent.replacement_job, ctx.now())?;
    Ok(wrote)
}

/// Why `intent` no longer matches the live flow state, or `None` when the
/// replacement may still be restored. Four checks, in the order the flow
/// can move under a frozen intent: the task must still be a step of the
/// intent's own run and node, the task must still be open, the intent must
/// still be the newest spend of its step input, and an indexed replacement
/// must still be at the attempt it was frozen at.
fn stale_reason(
    store: &PumpStore,
    intent: &RecoveryIntentRow,
) -> Result<Option<String>, VivariumError> {
    let Some(step) = store.step_by_task(&intent.task)? else {
        return Ok(Some(format!("step task {} is unknown", intent.task)));
    };
    if intent.step_job.is_some() && (step.run != intent.run || step.node != intent.node) {
        return Ok(Some(format!(
            "step task {} moved from run {} node {} to run {} node {}",
            intent.task, intent.run, intent.node, step.run, step.node
        )));
    }
    if matches!(step.state, StepState::Done | StepState::Failed) {
        return Ok(Some(format!("step task {} is closed ({:?})", intent.task, step.state)));
    }
    let head = store.recovery_allowance(&intent.task, &intent.input)?;
    if head != intent.ordinal {
        return Ok(Some(format!(
            "step input {:?} advanced to retry {head}; the intent is ordinal {}",
            intent.input, intent.ordinal
        )));
    }
    if let Some(bind) = &intent.step_job {
        let last = store.last_attempt(&intent.task)?;
        if last != bind.attempt {
            return Ok(Some(format!(
                "step task {} is on attempt {last}; the intent froze attempt {}",
                intent.task, bind.attempt
            )));
        }
    }
    Ok(None)
}

/// The frozen intent for the `ordinal`th transient redispatch of `job`'s
/// step input: the deterministic replacement id, its absolute deadline (the
/// class's delay plus the shared jitter, selected once here and persisted
/// with the row), and the complete request — a [`PoolJob::rerun_spec`] copy
/// of the failed job, renamed and re-stamped with recovery metadata.
fn intent_row(
    ctx: &Ctx,
    job: &PoolJob,
    binding: Option<&StepJobRow>,
    class: &str,
    task: &str,
    input: &str,
    ordinal: u32,
) -> RecoveryIntentRow {
    let replacement = format!("{input}-tr{ordinal}");
    let deadline = ctx.now() + DELAYS[(ordinal - 1) as usize] + transient::jitter(&replacement);
    let mut request = job.rerun_spec(&replacement);
    stamp_metadata(
        &mut request.payload,
        &RecoveryMeta {
            input: input.to_owned(),
            retry: ordinal,
            class: class.to_owned(),
        },
    );
    RecoveryIntentRow {
        need: payload_text(job, "need").unwrap_or_default().to_owned(),
        run: payload_run(job),
        node: payload_text(job, "node").unwrap_or_default().to_owned(),
        task: task.to_owned(),
        input: input.to_owned(),
        failed_job: job.id.clone(),
        replacement_job: replacement.clone(),
        kind: job.kind.clone(),
        ordinal,
        reason: class.to_owned(),
        deadline,
        request,
        step_job: binding.map(|row| row.again(&replacement)),
        state: recovery_state::PENDING.to_owned(),
        applied_at: None,
    }
}

/// The [`Decision::AlreadyRecorded`] answer for a stored intent.
fn recorded(intent: &RecoveryIntentRow) -> Decision {
    Decision::AlreadyRecorded {
        replacement: intent.replacement_job.clone(),
        deadline: intent.deadline,
    }
}

/// How many transient ordinals `job`'s step input has spent: the higher of
/// the ordinal its payload inherited and the store's durable allowance. The
/// store is the ledger; the metadata is what a correction or rerun carried.
fn spent(
    store: &PumpStore,
    job: &PoolJob,
    task: &str,
    input: &str,
) -> Result<u32, VivariumError> {
    Ok(retries_spent(&job.payload).max(store.recovery_allowance(task, input)?))
}

/// The step input `job` ran on: the recovery metadata's identity when the
/// chain already carries one — every payload-copying policy inherits it —
/// else the job-id base the first transient retry names.
fn step_input(job: &PoolJob) -> String {
    match metadata(&job.payload) {
        Some(meta) => meta.input,
        None => base_id(&job.id).to_owned(),
    }
}

/// The mailspace task the intent is accounted on: the step-job binding's
/// task for an indexed job, else the job's own task.
fn task_of(job: &PoolJob, binding: Option<&StepJobRow>) -> String {
    match binding {
        Some(row) => row.task.clone(),
        None => job.task.clone().unwrap_or_default(),
    }
}

/// A text payload field, or `None` when absent or not a string.
fn payload_text<'a>(job: &'a PoolJob, key: &str) -> Option<&'a str> {
    job.payload.get(key).and_then(serde_json::Value::as_str)
}

/// The payload's `run` number, or `0` when absent or not a number.
fn payload_run(job: &PoolJob) -> u32 {
    job.payload
        .get("run")
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |raw| u32::try_from(raw).unwrap_or(0))
}

#[cfg(test)]
#[path = "intent_test.rs"]
mod tests;
