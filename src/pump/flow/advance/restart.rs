//! Idempotent restart of a flow run.
//!
//! A restart sets the old run `restarting`, starts the next run (which is
//! itself resumable), closes every open task of the old run `superseded` with
//! its sentinel last, then sets the old run `superseded`. A failure at any
//! write leaves the old run `restarting`; the [`Restarts`] tick phase resumes
//! it and drops the old run's queued jobs.

use std::collections::BTreeSet;

use crate::error::VivariumError;
use crate::pump::flow::advance::start::{RUN_STARTING, SENTINEL_NODE, start};
use crate::pump::flow::def::VERDICT_SUPERSEDED;
use crate::pump::flow::fanout::{close_slice, is_slice_row};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStatus, JobStore};
use crate::pump::store::PumpStore;

/// Run state: a restart of this run began and has not finished.
pub(crate) const RUN_RESTARTING: &str = "restarting";
/// Run state: replaced by a newer run.
const RUN_SUPERSEDED: &str = "superseded";
/// Run state: withdrawn.
const RUN_WITHDRAWN: &str = "withdrawn";
/// Run state: a run still being advanced.
const RUN_LIVE: &str = "live";
/// Exception kind: a restart stayed unfinished too long.
pub(crate) const RESTART_STUCK: &str = "flow-run-restart-stuck";
/// Seconds a run may stay `restarting` before the alarm.
const RESTART_STUCK_SECS: u64 = 600;

/// Registry hook: the restart phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(Restarts));
}

/// Restart `need` from `old` at `entry`: set `old` restarting, start the next
/// run, close the old run's open tasks `superseded` (the sentinel last), set
/// `old` superseded. Safe to repeat after a failure: see [`resume`].
///
/// # Errors
/// Returns a [`VivariumError`] when the start, a close or a store write fails.
pub(crate) fn restart<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    old: &RunRow,
    entry: &str,
) -> Result<RunRow, VivariumError> {
    store.set_run_state(&old.need, old.run, RUN_RESTARTING)?;
    let next = start(ctx, store, mail, &old.need, Some(entry))?;
    supersede(store, mail, old)?;
    Ok(next)
}

/// Finish a restart left `restarting`: the newer run of the same need
/// (starting or live) is the restart target; resume it if it is still
/// starting, then finish the closes.
///
/// # Errors
/// Returns a [`VivariumError`] when no newer run exists (the entry is
/// unknown), or a start, close or store call fails.
pub(crate) fn resume<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    old: &RunRow,
) -> Result<(), VivariumError> {
    let newer = store
        .runs_of(&old.need)?
        .into_iter()
        .filter(|row| row.run > old.run && (row.state == RUN_STARTING || row.state == RUN_LIVE))
        .max_by_key(|row| row.run)
        .ok_or_else(|| {
            VivariumError::Message(format!(
                "need {} r{}: restarting but no newer run exists, so its entry is unknown",
                old.need, old.run
            ))
        })?;
    if newer.state == RUN_STARTING {
        start(ctx, store, mail, &old.need, Some(&newer.entry))?;
    }
    supersede(store, mail, old)
}

/// Close every open task of `old` `superseded`, the sentinel last, then set the
/// run `superseded`.
fn supersede<M: MailWriter + MailReader>(
    store: &PumpStore,
    mail: &mut M,
    old: &RunRow,
) -> Result<(), VivariumError> {
    let rows = store.steps_of(&old.need, old.run)?;
    let (sentinels, steps): (Vec<StepRow>, Vec<StepRow>) =
        rows.into_iter().partition(|r| r.node == SENTINEL_NODE);
    for row in steps.iter().chain(&sentinels) {
        close_open(store, mail, row)?;
    }
    store.set_run_state(&old.need, old.run, RUN_SUPERSEDED)
}

/// Close `row`'s task if it is open; mark a closed task's row done.
fn close_open<M: MailWriter + MailReader>(
    store: &PumpStore,
    mail: &mut M,
    row: &StepRow,
) -> Result<(), VivariumError> {
    let state = mail.task_state(&row.task)?;
    if state.status == TaskStatus::Open {
        if is_slice_row(row) {
            return close_slice(store, mail, &row.task, VERDICT_SUPERSEDED);
        }
        mail.close_task(&row.task, VERDICT_SUPERSEDED, None, None)?;
        return store.set_step_state(&row.task, StepState::Done, Some(VERDICT_SUPERSEDED));
    }
    if row.state != StepState::Done {
        store.set_step_state(&row.task, StepState::Done, state.verdict.as_deref())?;
    }
    Ok(())
}

/// The tick phase: resume restarts, then drop the queued jobs of dead runs.
struct Restarts;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for Restarts {
    fn name(&self) -> &'static str {
        "restarts"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        for run in store.runs_in_state(RUN_RESTARTING)? {
            resume_one(ctx, store, mail, &run);
        }
        prune_dead_jobs(ctx, st, store)
    }
}

/// Resume one restart; past [`RESTART_STUCK_SECS`] raise the stuck alarm once.
fn resume_one<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
) {
    let Err(error) = resume(ctx, store, mail, run) else {
        return;
    };
    let unit = format!("{} r{}", run.need, run.run);
    ctx.log(&format!("flow tick: restart of {unit} failed: {error}"));
    let since = match store.run_state_at(&run.need, run.run) {
        Ok(Some(at)) => Some(u64::try_from(at).unwrap_or(0)),
        _ => None,
    };
    if since.is_some_and(|at| ctx.now().saturating_sub(at) > RESTART_STUCK_SECS) {
        raise_once(
            ctx,
            RESTART_STUCK,
            &unit,
            &format!(
                "need {} run r{} cannot finish its restart: {error}",
                run.need, run.run
            ),
        );
    }
}

/// Remove every Pending job whose step belongs to a superseded or withdrawn
/// run. Dispatched jobs finish on their own; their results are ignored because
/// their step rows are closed.
fn prune_dead_jobs<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
) -> Result<(), VivariumError> {
    let mut dead: BTreeSet<String> = BTreeSet::new();
    for state in [RUN_SUPERSEDED, RUN_WITHDRAWN] {
        for run in store.runs_in_state(state)? {
            let rows = store.steps_of(&run.need, run.run)?;
            dead.extend(rows.into_iter().map(|r| r.task));
        }
    }
    if dead.is_empty() {
        return Ok(());
    }
    let mut doomed = Vec::new();
    for (id, job) in st.jobs() {
        if job.status == JobStatus::Pending
            && let Some(row) = store.step_job(id)?
            && dead.contains(&row.task)
        {
            doomed.push(id.clone());
        }
    }
    for id in doomed {
        if st.jobs_mut().remove(&id).is_some() {
            ctx.log(&format!(
                "flow tick: removed pending job {id} of a dead run"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "restart_test.rs"]
mod tests;
