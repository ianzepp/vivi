//! The step-pending phase: no silent holds.
//!
//! A step row left `Pending` (no handler started it) or a step job that never
//! leaves `Pending` (no pool took it) past its budget plus a grace raises
//! `flow-step-pending` once. Step rows carry no time, so the phase keeps a
//! process-local first-seen clock for them; jobs carry `queued_at`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::error::VivariumError;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::reslice;
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStatus, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Exception kind of a step or job held Pending past its budget.
pub const STEP_PENDING: &str = "flow-step-pending";

/// Seconds past the budget before the hold is raised.
const GRACE_SECS: u64 = 600;

struct StepPending;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for StepPending {
    fn name(&self) -> &'static str {
        "step-pending"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        _mail: &mut M,
    ) -> Result<(), VivariumError> {
        for run in store.live_runs()? {
            let steps = store.steps_of(&run.need, run.run)?;
            for step in &steps {
                if step.state == StepState::Pending && step.node != SENTINEL_NODE {
                    check_step(ctx, &run, step);
                }
            }
            for job in st.jobs().values() {
                if job.status == JobStatus::Pending && belongs(store, &run, &steps, job)? {
                    check_job(ctx, &run, job);
                }
            }
        }
        Ok(())
    }
}

/// Registry hook: the step-pending phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(StepPending));
}

/// Raise when a Pending step row has been seen longer than budget plus grace.
fn check_step(ctx: &Ctx, run: &RunRow, step: &StepRow) {
    let now = ctx.now();
    let first = first_seen(ctx, &step.task, now);
    let limit = ctx.cfg.dispatch.budget + GRACE_SECS;
    if now.saturating_sub(first) > limit {
        raise_once(
            ctx,
            STEP_PENDING,
            &step.task,
            &format!(
                "need {} r{} node {} task {}: step Pending for over {limit}s, no handler started it",
                run.need, run.run, step.node, step.task
            ),
        );
    }
}

/// Raise when a Pending job has waited longer than its budget plus grace.
fn check_job(ctx: &Ctx, run: &RunRow, job: &PoolJob) {
    let waited = ctx.now().saturating_sub(job.queued_at);
    let limit = job.budget + GRACE_SECS;
    if waited > limit {
        raise_once(
            ctx,
            STEP_PENDING,
            &job.id,
            &format!(
                "need {} r{} job {} (task {}): Pending for {waited}s, over {limit}s, no pool took it",
                run.need,
                run.run,
                job.id,
                job.task.as_deref().unwrap_or("none")
            ),
        );
    }
}

/// Whether a job is one of this run's: a step job of one of its steps, or a
/// reslice job whose payload names this need and run.
fn belongs(
    store: &PumpStore,
    run: &RunRow,
    steps: &[StepRow],
    job: &PoolJob,
) -> Result<bool, VivariumError> {
    if job.kind == reslice::KIND {
        let need = job.payload.get("need").and_then(serde_json::Value::as_str);
        let number = job.payload.get("run").and_then(serde_json::Value::as_u64);
        return Ok(need == Some(run.need.as_str()) && number == Some(u64::from(run.run)));
    }
    Ok(store
        .step_job(&job.id)?
        .is_some_and(|row| row.run == run.run && steps.iter().any(|step| step.task == row.task)))
}

/// The first time this process saw `task` Pending, recording `now` if new.
fn first_seen(ctx: &Ctx, task: &str, now: u64) -> u64 {
    static SEEN: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *seen
        .entry(format!("{}\u{0}{task}", ctx.home.display()))
        .or_insert(now)
}

#[cfg(test)]
#[path = "step_pending_test.rs"]
mod tests;
