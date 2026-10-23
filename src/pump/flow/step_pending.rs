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
use crate::pump::flow::fanout::{VERDICT_MERGED, VERDICT_NO_DEFECT, is_slice_row};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, TaskStatus};
use crate::pump::flow::reslice;
use crate::pump::flow::scope::ScopeState;
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
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        for run in store.live_runs()? {
            let steps = store.steps_of(&run.need, run.run)?;
            for step in &steps {
                if step.state != StepState::Pending || step.node == SENTINEL_NODE {
                    continue;
                }
                if is_slice_row(step) && slice_waiting(store, &*mail, step)? {
                    forget(ctx, &step.task);
                } else {
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

/// Whether a slice row waits by design: a dependency is not closed merged or
/// no-defect, or its scope request is waiting or being reworked.
fn slice_waiting<M: MailReader + ?Sized>(
    store: &PumpStore,
    mail: &M,
    step: &StepRow,
) -> Result<bool, VivariumError> {
    if matches!(
        store.scope_state(&step.task)?,
        Some(ScopeState::Wait | ScopeState::Rework)
    ) {
        return Ok(true);
    }
    let Some(link) = store.slice_link(&step.task)? else {
        return Ok(false);
    };
    for dep in &link.deps {
        let state = mail.task_state(dep)?;
        let built = state.status == TaskStatus::Done
            && matches!(
                state.verdict.as_deref(),
                Some(VERDICT_MERGED | VERDICT_NO_DEFECT)
            );
        if !built {
            return Ok(true);
        }
    }
    Ok(false)
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

/// Raise when a Pending job has waited longer than its budget plus grace. A
/// held job already raised `job-dispatch-refused`, so it raises nothing here.
fn check_job(ctx: &Ctx, run: &RunRow, job: &PoolJob) {
    if job.held.is_some() {
        return;
    }
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

fn seen_map() -> std::sync::MutexGuard<'static, HashMap<String, u64>> {
    static SEEN: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn seen_key(ctx: &Ctx, task: &str) -> String {
    format!("{}\u{0}{task}", ctx.home.display())
}

/// The first time this process saw `task` Pending, recording `now` if new.
fn first_seen(ctx: &Ctx, task: &str, now: u64) -> u64 {
    *seen_map().entry(seen_key(ctx, task)).or_insert(now)
}

/// Drop the first-seen time of `task`: its budget restarts when it is ready.
fn forget(ctx: &Ctx, task: &str) {
    seen_map().remove(&seen_key(ctx, task));
}

#[cfg(test)]
#[path = "step_pending_test.rs"]
mod tests;
