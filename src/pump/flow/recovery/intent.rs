//! Durable allocation and replay of bounded transient replacements.
//!
//! [`allocate`] is the collector-side freeze. A collected job the classifier
//! names transient and the publication-safety gate permits spends one of the
//! step input's three redispatch ordinals and freezes an intent: the
//! deterministic replacement id, the complete [`PoolJob::rerun_spec`] copy
//! of the request, and the absolute deadline — 60, 120 or 240 seconds plus
//! [`transient::jitter`] — the replacement must be enqueued inside. The
//! deadline is selected once and persisted with the intent; nothing later
//! moves it.
//!
//! The step input's identity is the node and task the job ran on, never the
//! job-id chain: ordinary work attempts, envelope corrections and
//! budget-retry markers have their own policies and cannot replenish the
//! allowance, and a correction of a retried job draws the same counter its
//! source drew. The transient `-tr<N>` ordinal stays separate from both.
//!
//! The intent is written before every enqueue, index and application
//! effect. The replacement itself is minted later, through [`replay`], once
//! the deadline has passed: validate, enqueue the frozen request, index the
//! step binding, stamp the intent applied. [`replay`] is the crash path too
//! — an earlier process that marked the enqueue complete but died before the
//! `State` save left an intent whose replacement is missing, and replay
//! restores it from the frozen request without duplicating a present job or
//! resetting the deadline.
//!
//! Staged like [`safety`]: the collector that consumes the decisions lands
//! in a later slice.
#![cfg_attr(not(test), allow(dead_code))]

use crate::error::VivariumError;
use crate::pump::flow::index::{RunRow, StepJobRow, StepState, run_state};
use crate::pump::flow::recovery::{RecoveryMeta, next_replacement_id, safety};
use crate::pump::flow::recovery::{stamp_metadata, transient_class};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec, JobStore, PoolJob};
use crate::pump::store::PumpStore;
use crate::pump::store::recovery::{RecoveryIntentRow, recovery_state};
use crate::pump::transient;

/// The transient redispatches one step input may spend; the next candidate
/// after the third is refused.
pub(crate) const MAX_RETRIES: u32 = 3;

/// The first retry's delay in seconds; each later ordinal doubles it, so
/// the three deadlines sit at 60, 120 and 240 seconds plus jitter.
pub(crate) const FIRST_DELAY_SECS: u64 = 60;

/// Why a restore was refused: the intent's owner run is gone.
const RUN_GONE: &str = "run-gone";
/// Why a restore was refused: the owner run is no longer live.
const RUN_NOT_LIVE: &str = "run-not-live";
/// Why a restore was refused: the step task is gone.
const TASK_GONE: &str = "task-gone";
/// Why a restore was refused: the step task is done or failed.
const TASK_CLOSED: &str = "task-closed";
/// Why a restore was refused: the step has a newer attempt.
const ATTEMPT_STALE: &str = "attempt-stale";
/// Why a restore was refused: a newer ordinal was spent on the same input.
const INPUT_SUPERSEDED: &str = "input-superseded";

/// What a collector should do with a collected failed job.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decision {
    /// The job shows no transient class: normal collection owns it.
    NotEligible,
    /// A retry intent is recorded for the step input, and the replacement
    /// will be enqueued when its deadline passes — never by the collection
    /// itself. `recorded` is false when this call minted the intent and
    /// true when the failed job already had one: the stored deadline,
    /// ordinal and request are authoritative, and no ordinal was spent.
    Queued {
        /// The frozen (or retrieved) intent.
        intent: RecoveryIntentRow,
        /// Whether the intent already existed for this failed job.
        recorded: bool,
    },
    /// The step input has spent all [`MAX_RETRIES`] redispatches.
    Exhausted {
        /// The ordinals already spent on the input.
        spent: u32,
    },
    /// The publication-safety gate refused a replacement: `code` says why,
    /// and the step stays with its existing collection or hold path.
    Unsafe {
        /// The gate's diagnostic code word.
        code: &'static str,
    },
}

/// The outcome of replaying one intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Replay {
    /// The replacement was missing and is restored: enqueued, indexed and
    /// stamped applied from the frozen request.
    Restored {
        /// The replacement job id that was enqueued.
        replacement: String,
    },
    /// The replacement is already in the pool: nothing was duplicated and
    /// the stored deadline is untouched.
    Present,
    /// The deadline has not passed; the replacement stays unenqueued.
    Waiting,
    /// Validation refused the restore; `why` is a stable word naming the
    /// stale coordinate.
    Refused {
        /// Which coordinate is no longer current.
        why: &'static str,
    },
}

/// Classify a collected failed job, pass the publication-safety gate, and
/// freeze — or retrieve — its transient-retry intent.
///
/// `binding` is the step-job row of an indexed step job; independently
/// collected owned jobs pass `None` and keep their own collection
/// ownership: only the recovery metadata is added to their payload.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read or written.
pub(crate) fn allocate(
    ctx: &Ctx,
    store: &PumpStore,
    run: &RunRow,
    binding: Option<&StepJobRow>,
    job: &PoolJob,
    now: u64,
) -> Result<Decision, VivariumError> {
    let Some(class) = transient_class(job) else {
        return Ok(Decision::NotEligible);
    };
    match safety::decide(ctx, job, class) {
        safety::RecoverySafety::Safe { .. } => {}
        safety::RecoverySafety::Published { code } | safety::RecoverySafety::Unclear { code } => {
            return Ok(Decision::Unsafe { code });
        }
    }
    freeze(store, run, binding, job, class, now)
}

/// Spend the input's next ordinal and write the intent, or return the
/// intent an earlier collection of the same failed job already froze.
fn freeze(
    store: &PumpStore,
    run: &RunRow,
    binding: Option<&StepJobRow>,
    job: &PoolJob,
    class: &str,
    now: u64,
) -> Result<Decision, VivariumError> {
    // A repeated collection retrieves the existing intent before any
    // allowance is read, so a replayed failure can never spend an ordinal
    // or trip the cap on its own stored retry.
    if let Some(existing) = store.recovery_intent_of_failed_job(&job.id)? {
        return Ok(Decision::Queued {
            intent: existing,
            recorded: true,
        });
    }
    let task = step_task(binding, job);
    let spent = store.recovery_allowance(&task, &task)?;
    if spent >= MAX_RETRIES {
        return Ok(Decision::Exhausted { spent });
    }
    let ordinal = spent + 1;
    let replacement = next_replacement_id(&job.id);
    let deadline = deadline_at(now, ordinal, &replacement);
    let request = request_of(run, binding, job, &task, ordinal, class, &replacement);
    let candidate = intent_of(
        run,
        binding,
        job,
        class,
        &task,
        ordinal,
        deadline,
        request,
        &replacement,
    );
    let intent = store.insert_recovery_intent(&candidate)?;
    Ok(Decision::Queued {
        intent,
        recorded: false,
    })
}

/// Enqueue effects of one intent, replay-safe: a present replacement is
/// never duplicated, a missing one is restored from the frozen request
/// once the deadline has passed and the intent's coordinates still hold.
///
/// # Errors
/// Returns [`VivariumError`] when the store or the job map cannot be
/// written.
pub(crate) fn replay(
    st: &mut impl JobStore,
    store: &PumpStore,
    intent: &RecoveryIntentRow,
    now: u64,
) -> Result<Replay, VivariumError> {
    if st.jobs().contains_key(&intent.replacement_job) {
        // Settle the applied stamp a crash may have left pending; never
        // duplicate the present job, never move the stored deadline.
        store.mark_recovery_applied(&intent.replacement_job, now)?;
        return Ok(Replay::Present);
    }
    if let Some(why) = stale_reason(store, intent)? {
        return Ok(Replay::Refused { why });
    }
    if now < intent.deadline {
        return Ok(Replay::Waiting);
    }
    restore(st, store, intent, now)?;
    Ok(Replay::Restored {
        replacement: intent.replacement_job.clone(),
    })
}

/// Replay every pending intent in deadline order; the driver calls this on
/// each tick. Applied intents are not listed by the store — a crash between
/// the applied stamp and the `State` save is repaired through the
/// re-collection path, which hands its retrieved intent to [`replay`].
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be read or written.
pub(crate) fn replay_pending(
    st: &mut impl JobStore,
    store: &PumpStore,
    now: u64,
) -> Result<Vec<(RecoveryIntentRow, Replay)>, VivariumError> {
    let mut answers = Vec::new();
    for intent in store.unfinished_recovery_intents()? {
        let answer = replay(st, store, &intent, now)?;
        answers.push((intent, answer));
    }
    Ok(answers)
}

/// Whether the intent's frozen coordinates no longer hold. The input check
/// applies to both shapes — a newer ordinal on the same step input means a
/// later intent owns the recovery; the run, task and attempt checks need an
/// indexed step job to name their rows.
fn stale_reason(
    store: &PumpStore,
    intent: &RecoveryIntentRow,
) -> Result<Option<&'static str>, VivariumError> {
    let spent = store.recovery_allowance(&intent.task, &intent.input)?;
    if spent != intent.ordinal {
        return Ok(Some(INPUT_SUPERSEDED));
    }
    let Some(row) = intent.step_job.as_ref() else {
        return Ok(None);
    };
    let live = store
        .runs_of(&intent.need)?
        .into_iter()
        .find(|candidate| candidate.run == intent.run);
    let Some(run) = live else {
        return Ok(Some(RUN_GONE));
    };
    if run.state != run_state::LIVE {
        return Ok(Some(RUN_NOT_LIVE));
    }
    let Some(step) = store.step_by_task(&intent.task)? else {
        return Ok(Some(TASK_GONE));
    };
    if matches!(step.state, StepState::Done | StepState::Failed) {
        return Ok(Some(TASK_CLOSED));
    }
    if store.last_attempt(&intent.task)? != row.attempt {
        return Ok(Some(ATTEMPT_STALE));
    }
    Ok(None)
}

/// The restore itself: enqueue the frozen request, index the binding, then
/// stamp the intent applied — the intent was written first, so a crash
/// before any of these effects is replayable and a crash after any of them
/// is idempotent here.
fn restore(
    st: &mut impl JobStore,
    store: &PumpStore,
    intent: &RecoveryIntentRow,
    now: u64,
) -> Result<(), VivariumError> {
    jobs::enqueue(st, intent.request.clone())?;
    if let Some(row) = intent.step_job.as_ref() {
        // The index row may already exist when the crash landed after the
        // index write but before the State save.
        if store.step_job(&intent.replacement_job)?.is_none() {
            store.insert_step_job(&intent.need, &row.again(&intent.replacement_job))?;
        }
    }
    store.mark_recovery_applied(&intent.replacement_job, now)
}

/// The absolute deadline of `ordinal`: the doubled delay plus the
/// deterministic jitter keyed by the replacement id.
fn deadline_at(now: u64, ordinal: u32, seed: &str) -> u64 {
    now + (FIRST_DELAY_SECS << u64::from(ordinal - 1)) + transient::jitter(seed)
}

/// The step task an intent is accounted on: the binding's task for an
/// indexed step job, the job's own task handle for an owned job. This is
/// also the step-input identity: deliberately free of the job-id chain and
/// of the attempt number, so ordinary attempts, corrections and budget
/// retries all draw the same allowance their input already spent, and a
/// correction of a retried job draws the counter its source drew.
fn step_task(binding: Option<&StepJobRow>, job: &PoolJob) -> String {
    binding.map_or_else(
        || job.task.clone().unwrap_or_default(),
        |row| row.task.clone(),
    )
}

/// The frozen replacement request: [`PoolJob::rerun_spec`] is the one copy,
/// so task, every execution input, the payload and the spent budget-retry
/// marker survive; only the id changes and the recovery metadata is added.
/// An indexed retry serves the failed job's own attempt — the transient
/// ordinal stays separate from the work-attempt and correction policies.
fn request_of(
    run: &RunRow,
    binding: Option<&StepJobRow>,
    job: &PoolJob,
    input: &str,
    ordinal: u32,
    class: &str,
    replacement: &str,
) -> JobSpec {
    let mut spec = job.rerun_spec(replacement);
    if let Some(row) = binding {
        row.again(replacement).stamp(&run.need, &mut spec.payload);
    }
    stamp_metadata(
        &mut spec.payload,
        &RecoveryMeta {
            input: input.to_owned(),
            retry: ordinal,
            class: class.to_owned(),
        },
    );
    spec
}

/// The intent row for the fresh allocation; [`recovery_state::PENDING`]
/// until the replacement's effects land. `task` doubles as the input
/// identity (see [`step_task`]).
#[allow(clippy::too_many_arguments)]
fn intent_of(
    run: &RunRow,
    binding: Option<&StepJobRow>,
    job: &PoolJob,
    class: &str,
    task: &str,
    ordinal: u32,
    deadline: u64,
    request: JobSpec,
    replacement: &str,
) -> RecoveryIntentRow {
    RecoveryIntentRow {
        need: run.need.clone(),
        run: run.run,
        node: binding.map_or(String::new(), |row| row.node.clone()),
        task: task.to_owned(),
        input: task.to_owned(),
        failed_job: job.id.clone(),
        replacement_job: replacement.to_owned(),
        kind: job.kind.clone(),
        ordinal,
        reason: class.to_owned(),
        deadline,
        request,
        step_job: binding.map(|row| row.again(replacement)),
        state: recovery_state::PENDING.to_owned(),
        applied_at: None,
    }
}

#[cfg(test)]
#[path = "intent_test.rs"]
mod tests;
