//! Queues one slice merge per need, from the need-branch tip.
//!
//! A built slice waits on its step row with verdict [`MERGE_QUEUED`]. Each
//! tick starts the first queued slice of a need that has no slice merge job
//! pending or dispatched, so the need branch moves one merge at a time.

use crate::error::VivariumError;
use crate::pump::flow::fanout;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::slice_merge::spec::{self, KIND};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobStatus, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Verdict of a built slice row waiting for its merge onto the need branch.
pub(crate) const MERGE_QUEUED: &str = "merge-queued";

/// Exception kind for a queued slice that cannot start its merge.
const MERGE_FAILED: &str = "flow-slice-merge-failed";

/// Drop dead slice merge jobs, then start one merge per need.
///
/// # Errors
/// Returns [`VivariumError`] when the flow index cannot be read or written.
pub(crate) fn run<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
) -> Result<(), VivariumError> {
    let runs = store.live_runs()?;
    drop_dead(ctx, st, store, &runs)?;
    for run in &runs {
        if in_flight(st, &run.need) {
            continue;
        }
        let Some(row) = first_queued(store, run)? else {
            continue;
        };
        if let Err(error) = start(ctx, st, store, mail, run, &row) {
            raise_once(
                ctx,
                MERGE_FAILED,
                &row.task,
                &format!(
                    "slice {} of {} cannot start its merge: {error}",
                    row.node, run.need
                ),
            );
        }
    }
    Ok(())
}

fn in_flight<S: JobStore>(st: &S, need: &str) -> bool {
    st.jobs().values().any(|job| {
        job.kind == KIND
            && matches!(job.status, JobStatus::Pending | JobStatus::Dispatched)
            && need_of(job) == Some(need)
    })
}

fn need_of(job: &PoolJob) -> Option<&str> {
    job.payload.get("need").and_then(serde_json::Value::as_str)
}

fn first_queued(store: &PumpStore, run: &RunRow) -> Result<Option<StepRow>, VivariumError> {
    Ok(store.steps_of(&run.need, run.run)?.into_iter().find(|row| {
        fanout::is_slice_row(row)
            && row.state == StepState::Waiting
            && row.verdict.as_deref() == Some(MERGE_QUEUED)
    }))
}

fn start<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    row: &StepRow,
) -> Result<(), VivariumError> {
    let hand = spec::hand_job_of(st, &row.task).ok_or_else(|| {
        VivariumError::Other(format!("slice {} has no finished hand job", row.node))
    })?;
    let job = spec::spec(ctx, store, &*mail, run, row, &hand)?;
    jobs::enqueue(st, job)?;
    store.set_step_state(&row.task, StepState::Running, None)
}

/// True while the slice row of `task` can still use a merge.
fn slice_open(store: &PumpStore, task: &str) -> Result<bool, VivariumError> {
    Ok(store.step_by_task(task)?.is_some_and(|row| {
        matches!(
            row.state,
            StepState::Pending | StepState::Running | StepState::Waiting
        )
    }))
}

fn run_live(job: &PoolJob, runs: &[RunRow]) -> bool {
    let number = job.payload.get("run").and_then(serde_json::Value::as_u64);
    runs.iter().any(|run| {
        need_of(job) == Some(run.need.as_str()) && number.is_none_or(|n| n == u64::from(run.run))
    })
}

/// Remove Pending slice merge jobs whose slice or run is gone. They have no
/// step_jobs row, so the restart phase does not see them.
fn drop_dead<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    runs: &[RunRow],
) -> Result<(), VivariumError> {
    let mut dead = Vec::new();
    for job in st.jobs().values() {
        if job.kind != KIND || job.status != JobStatus::Pending {
            continue;
        }
        let open = match job.task.as_deref() {
            Some(task) => slice_open(store, task)?,
            None => false,
        };
        if !open || !run_live(job, runs) {
            dead.push(job.id.clone());
        }
    }
    for id in dead {
        st.jobs_mut().remove(&id);
        ctx.log(&format!(
            "slice merge: removed pending job {id} of a closed slice or superseded run"
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "queue_test.rs"]
mod tests;
