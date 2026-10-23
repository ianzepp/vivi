//! Queues one slice merge per need, from the need-branch tip.
//!
//! A built slice waits on its step row with verdict [`MERGE_QUEUED`]. Each
//! tick starts the first queued slice of a need that has no slice merge job
//! pending or dispatched, so the need branch moves one merge at a time.

use crate::error::VivariumError;
use crate::pump::flow::fanout;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::need_branch;
use crate::pump::flow::slice_merge::spec::{self, KIND};
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::flow::verify;
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
            // A start error holds the slice for the operator: it is never
            // retried tick after tick.
            store.set_step_state(&row.task, StepState::Waiting, Some(OPERATOR_HOLD))?;
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
    if let Some(tip) = already_merged(ctx, &*mail, run, row, &hand)? {
        // Merged by ancestry on the need branch, a machine fact: no job and
        // no check.
        return fanout::close_slice_merged(
            store,
            mail,
            &row.task,
            &need_branch::branch(&run.need),
            &tip,
        );
    }
    let try_no = merge_tries(st, &hand.id) + 1;
    let job = spec::spec(ctx, store, &*mail, run, row, &hand, try_no)?;
    let id = job.id.clone();
    jobs::enqueue(st, job)?;
    if !st.jobs().contains_key(&id) {
        return Err(VivariumError::Other(format!(
            "slice merge job {id} was not queued"
        )));
    }
    store.set_step_state(&row.task, StepState::Running, None)
}

/// How many slice merge jobs already ran for `hand_job`.
fn merge_tries<S: JobStore>(st: &S, hand_job: &str) -> u32 {
    let count = st
        .jobs()
        .values()
        .filter(|job| {
            job.kind == KIND
                && job
                    .payload
                    .get("hand_job")
                    .and_then(serde_json::Value::as_str)
                    == Some(hand_job)
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The need-branch tip when the hand's commit is already on the need branch.
fn already_merged<M: FlowMail>(
    ctx: &Ctx,
    mail: &M,
    run: &RunRow,
    row: &StepRow,
    hand: &PoolJob,
) -> Result<Option<String>, VivariumError> {
    let Some(sha) = spec::hand_commit(hand) else {
        return Ok(None);
    };
    let body = mail.task_body(&row.task)?;
    let target = fanout::target_of(&body)
        .ok_or_else(|| VivariumError::Parse(format!("slice task {} names no target", row.task)))?;
    let remote_ref = format!("refs/heads/{}", need_branch::branch(&run.need));
    verify::ref_contains(ctx, target, &remote_ref, &sha).map_err(VivariumError::Other)
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
