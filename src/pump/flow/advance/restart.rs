//! Idempotent restart of a flow run.
//!
//! A restart sets the old run `restarting`, starts the next run (which is
//! itself resumable), closes every open task of the old run `superseded` with
//! its sentinel last, then sets the old run `superseded`. A failure at any
//! write leaves the old run `restarting`; the [`Restarts`] tick phase resumes
//! it and drops the old run's queued jobs.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::VivariumError;
use crate::letter::records::NoteWord;
use crate::mailspace::pump_note::note_body;
use crate::pump::flow::advance::start::{SENTINEL_NODE, start};
use crate::pump::flow::def::VERDICT_SUPERSEDED;
use crate::pump::flow::fanout::{close_slice, is_slice_row};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStatus, JobStore, mark_applied};
use crate::pump::store::PumpStore;

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
    store.set_restart_entry(&old.need, old.run, entry)?;
    store.set_run_state(&old.need, old.run, run_state::RESTARTING)?;
    let next = start(ctx, store, mail, &old.need, Some(entry))?;
    supersede(store, mail, old)?;
    Ok(next)
}

/// Finish a restart left `restarting`: the newer run of the same need
/// (starting or live) is the restart target; resume it if it is still
/// starting, then finish the closes. With no newer run (the start failed
/// before it wrote a run row), start one at the entry [`restart`] recorded.
///
/// # Errors
/// Returns a [`VivariumError`] when no newer run exists and no entry was
/// recorded, or a start, close or store call fails.
pub(crate) fn resume<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    old: &RunRow,
) -> Result<(), VivariumError> {
    let newer = store
        .runs_of(&old.need)?
        .into_iter()
        .filter(|row| {
            row.run > old.run && (row.state == run_state::STARTING || row.state == run_state::LIVE)
        })
        .max_by_key(|row| row.run);
    match newer {
        Some(row) if row.state == run_state::STARTING => {
            start(ctx, store, mail, &old.need, Some(&row.entry))?;
        }
        Some(_) => {}
        None => {
            let entry = store.restart_entry(&old.need, old.run)?.ok_or_else(|| {
                VivariumError::Message(format!(
                    "need {} r{}: restarting but no newer run exists, so its entry is unknown",
                    old.need, old.run
                ))
            })?;
            start(ctx, store, mail, &old.need, Some(&entry))?;
        }
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
    store.set_run_state(&old.need, old.run, run_state::SUPERSEDED)
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
        for run in store.runs_in_state(run_state::RESTARTING)? {
            resume_one(ctx, store, mail, &run);
        }
        prune_dead_jobs(ctx, st, store, mail)
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

/// A superseded or withdrawn run with its step rows.
struct DeadRun {
    run: RunRow,
    rows: Vec<StepRow>,
}

impl DeadRun {
    /// The task of the run's sentinel row, if it has one.
    fn sentinel(&self) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| r.node == SENTINEL_NODE)
            .map(|r| r.task.as_str())
    }
}

/// Every superseded or withdrawn run with its step rows.
fn dead_runs(store: &PumpStore) -> Result<Vec<DeadRun>, VivariumError> {
    let mut dead = Vec::new();
    for state in [run_state::SUPERSEDED, run_state::WITHDRAWN] {
        for run in store.runs_in_state(state)? {
            let rows = store.steps_of(&run.need, run.run)?;
            dead.push(DeadRun { run, rows });
        }
    }
    Ok(dead)
}

/// Account for the jobs of dead runs.
///
/// Pending jobs are removed. Dispatched jobs finish on their own: there is no
/// `vivi pool cancel` verb, so the slot of a running job cannot be freed here.
/// Their ids are listed once on the run's sentinel (`pump-abandoned:`), and
/// their late terminal results are marked applied with one log line, so nothing
/// is posted, raised or opened for them. Jobs of other runs, and jobs with no
/// step-job row, are untouched.
fn prune_dead_jobs<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
) -> Result<(), VivariumError> {
    let dead = dead_runs(store)?;
    if dead.is_empty() {
        return Ok(());
    }
    let mut owner: BTreeMap<&str, usize> = BTreeMap::new();
    for (at, d) in dead.iter().enumerate() {
        owner.extend(d.rows.iter().map(|r| (r.task.as_str(), at)));
    }
    let mut doomed = Vec::new();
    let mut ended = Vec::new();
    let mut flying: Vec<Vec<String>> = vec![Vec::new(); dead.len()];
    for (id, job) in st.jobs() {
        let Some(row) = store.step_job(id)? else {
            continue;
        };
        let Some(&at) = owner.get(row.task.as_str()) else {
            continue;
        };
        match job.status {
            JobStatus::Pending => doomed.push(id.clone()),
            JobStatus::Dispatched => flying[at].push(id.clone()),
            status if job.applied_at.is_none() => {
                ended.push((id.clone(), at, format!("{status:?}")))
            }
            _ => {}
        }
    }
    for id in doomed {
        if st.jobs_mut().remove(&id).is_some() {
            ctx.log(&format!(
                "flow tick: removed pending job {id} of a dead run"
            ));
        }
    }
    for (id, at, status) in ended {
        if mark_applied(st, &id, ctx.now()) {
            let run = &dead[at].run;
            ctx.log(&format!(
                "abandoned job {id} of {} r{} ended {status}",
                run.need, run.run
            ));
        }
    }
    for (d, ids) in dead.iter().zip(flying) {
        if let Err(error) = list_abandoned(mail, d, ids) {
            ctx.log(&format!(
                "flow tick: abandoned list of {} r{} failed: {error}",
                d.run.need, d.run.run
            ));
        }
    }
    Ok(())
}

/// Post the dispatched `ids` of `dead` on its sentinel as one
/// abandoned note, leaving out ids an earlier reply already lists.
fn list_abandoned<M: FlowMail>(
    mail: &mut M,
    dead: &DeadRun,
    mut ids: Vec<String>,
) -> Result<(), VivariumError> {
    let Some(sentinel) = dead.sentinel() else {
        return Ok(());
    };
    if ids.is_empty() {
        return Ok(());
    }
    for (_, note) in mail.notes(sentinel, NoteWord::Abandoned)? {
        let listed: BTreeSet<&str> = note.prose.split_whitespace().collect();
        ids.retain(|id| !listed.contains(id.as_str()));
    }
    if ids.is_empty() {
        return Ok(());
    }
    ids.sort();
    mail.reply(
        sentinel,
        &note_body(NoteWord::Abandoned, None, &ids.join(" "))?,
    )
}

#[cfg(test)]
#[path = "restart_test.rs"]
mod tests;

#[cfg(test)]
#[path = "restart_entry_test.rs"]
mod entry_tests;
