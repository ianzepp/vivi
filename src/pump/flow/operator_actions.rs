//! Operator replan, withdraw and requeue on a need with an active flow run.
//!
//! The old pump applies these action letters to the old pipeline; a need the
//! flow engine runs must have them go to the flow instead. This tick phase
//! reads the letters the flow has not applied and, for each one whose need has
//! an active run (`has_active_run`):
//!
//! - `replan` restarts the live run at `plan`;
//! - `withdraw` closes the need's pump record (the tick withdraws a run whose
//!   record closed);
//! - `requeue` queues the step's newest job again, as `<job id>-q<n>`, at the
//!   same attempt; it is refused unless that job is terminal and the step is
//!   open (a done step is reopened by advance, never here). A Pending job held
//!   after repeated pool refusals is released in place instead: its hold and
//!   refusal count clear and the next dispatch offers it to a pool again;
//! - `answer` and `unlock` are left alone: other phases apply them.
//!
//! Every letter is applied or refused exactly once: its id goes into
//! `applied_actions` either way, and a refusal raises `flow-action-refused`.
//! Letters of needs with no active run are left for the old pump.

use std::collections::BTreeSet;

use crate::error::VivariumError;
use crate::mailspace::{PumpAction, PumpActionKind};
use crate::pump::flow::advance::restart::{self as run_restart};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepJobRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Exception kind of an operator action the flow cannot apply.
pub const ACTION_REFUSED: &str = "flow-action-refused";

/// The flow node a replan restarts at.
const REPLAN_ENTRY: &str = "plan";

/// Whether `need` has a run the flow engine is working on.
///
/// # Errors
/// Returns a [`VivariumError`] when the run index cannot be read.
pub(crate) fn has_active_run(store: &PumpStore, need: &str) -> Result<bool, VivariumError> {
    Ok(active_run(store, need)?.is_some())
}

/// The newest run of `need` in an active state.
fn active_run(store: &PumpStore, need: &str) -> Result<Option<RunRow>, VivariumError> {
    Ok(store
        .runs_of(need)?
        .into_iter()
        .filter(|run| {
            [
                run_state::STARTING,
                run_state::LIVE,
                run_state::RESTARTING,
                run_state::FINISHING,
            ]
            .contains(&run.state.as_str())
        })
        .max_by_key(|run| run.run))
}

struct OperatorActions;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for OperatorActions {
    fn name(&self) -> &'static str {
        "operator-actions"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        let Some(mailspace) = crate::pump::record::open_mailspace(ctx) else {
            return Ok(());
        };
        let applied = store.applied_action_ids()?;
        for action in mailspace.pump_pending_actions(&applied)? {
            let Some(run) = run_for(store, &action)? else {
                continue;
            };
            match apply_one(ctx, st, store, mail, &run, &action) {
                Ok(()) => ctx.log(&format!(
                    "operator action applied: {} on {}",
                    action.request.kind.as_str(),
                    action.need
                )),
                Err(e) => ctx.exception(ACTION_REFUSED, &action.need, &refusal(&action, &e)),
            }
            store.mark_action_applied(&action.id, ctx.now())?;
        }
        Ok(())
    }
}

/// Registry hook: the operator-actions phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(OperatorActions));
}

/// The active run a flow-owned action applies to; None leaves the letter alone.
fn run_for(store: &PumpStore, action: &PumpAction) -> Result<Option<RunRow>, VivariumError> {
    match action.request.kind {
        PumpActionKind::Replan | PumpActionKind::Withdraw | PumpActionKind::Requeue => {
            active_run(store, &action.need)
        }
        PumpActionKind::Answer | PumpActionKind::Unlock => Ok(None),
    }
}

/// The alarm text of a refused action.
fn refusal(action: &PumpAction, error: &VivariumError) -> String {
    format!(
        "{} action {} on need {}: {error}",
        action.request.kind.as_str(),
        action.id,
        action.need
    )
}

/// Apply one action to the need's active run.
fn apply_one<M: MailWriter + MailReader>(
    ctx: &Ctx,
    st: &mut impl JobStore,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    action: &PumpAction,
) -> Result<(), VivariumError> {
    match action.request.kind {
        PumpActionKind::Replan => {
            if run.state != run_state::LIVE {
                return Err(VivariumError::Message(format!(
                    "run r{} is {}, not live",
                    run.run, run.state
                )));
            }
            run_restart::restart(ctx, store, mail, run, REPLAN_ENTRY).map(drop)
        }
        PumpActionKind::Withdraw => mail.close_record(&action.need, "withdrawn"),
        PumpActionKind::Requeue => requeue(st, store, run, action.request.unit.as_deref()),
        PumpActionKind::Answer | PumpActionKind::Unlock => Err(VivariumError::Message(format!(
            "{} is applied by another phase",
            action.request.kind.as_str()
        ))),
    }
}

/// Queue the newest job of step task `unit` again at the same attempt.
fn requeue(
    st: &mut impl JobStore,
    store: &PumpStore,
    run: &RunRow,
    unit: Option<&str>,
) -> Result<(), VivariumError> {
    let task = unit.ok_or_else(|| VivariumError::Message("requeue names no step task".into()))?;
    let step = store
        .step_by_task(task)?
        .filter(|row| row.run == run.run)
        .ok_or_else(|| {
            VivariumError::Message(format!("{task} is not a step of run r{}", run.run))
        })?;
    if !matches!(
        step.state,
        StepState::Pending | StepState::Running | StepState::Waiting
    ) {
        return Err(VivariumError::Message(format!(
            "step {task} is {:?}, not open",
            step.state
        )));
    }
    if release_held(st, store, &run.need, task)? {
        return Ok(());
    }
    let (old, row) = newest_job(st, store, task)?
        .ok_or_else(|| VivariumError::Message(format!("step {task} has no job to requeue")))?;
    if !old.status.is_terminal() {
        return Err(VivariumError::Message(format!(
            "job {} is {:?}, not finished",
            old.id, old.status
        )));
    }
    let id = unused_id(st.jobs().keys().collect(), &old.id);
    let again = row.again(&id);
    let mut spec = copy_spec(&old, id);
    again.stamp(&run.need, &mut spec.payload);
    jobs::enqueue(st, spec)?;
    store.insert_step_job(&run.need, &again)?;
    store.set_step_state(&step.task, StepState::Running, None)
}

/// Release every held Pending job of step `task` in place: a job whose
/// payload names this task and need (a slice-merge job, a correction) or that
/// has a `step_jobs` row for it. True when any was released.
fn release_held(
    st: &mut impl JobStore,
    store: &PumpStore,
    need: &str,
    task: &str,
) -> Result<bool, VivariumError> {
    let mut ids = Vec::new();
    for job in st.jobs().values() {
        if job.status != JobStatus::Pending || job.held.is_none() {
            continue;
        }
        let stamped = job.payload["task"] == task && job.payload["need"] == need;
        if stamped || store.step_job(&job.id)?.is_some_and(|row| row.task == task) {
            ids.push(job.id.clone());
        }
    }
    for id in &ids {
        if let Some(job) = st.jobs_mut().get_mut(id) {
            job.held = None;
            job.dispatch_refusals = 0;
        }
    }
    Ok(!ids.is_empty())
}

/// The step's job at its highest attempt, with its `step_jobs` row.
fn newest_job(
    st: &impl JobStore,
    store: &PumpStore,
    task: &str,
) -> Result<Option<(PoolJob, StepJobRow)>, VivariumError> {
    let mut best: Option<(PoolJob, StepJobRow)> = None;
    for job in st.jobs().values() {
        let Some(row) = store.step_job(&job.id)? else {
            continue;
        };
        let newer = best
            .as_ref()
            .is_none_or(|(held, kept)| (row.attempt, &job.id) > (kept.attempt, &held.id));
        if row.task == task && newer {
            best = Some((job.clone(), row));
        }
    }
    Ok(best)
}

/// `<base>-q<n>` for the first `n` no job uses; a requeued id requeues its base.
fn unused_id(taken: BTreeSet<&String>, old: &str) -> String {
    let base = match old.rsplit_once("-q") {
        Some((base, tail)) if tail.parse::<u32>().is_ok() => base,
        _ => old,
    };
    (1..)
        .map(|n| format!("{base}-q{n}"))
        .find(|id| !taken.contains(id))
        .unwrap_or_default()
}

/// A pending copy of `job` under `id`.
fn copy_spec(job: &PoolJob, id: String) -> JobSpec {
    let mut spec = JobSpec::new(
        id,
        &job.kind,
        &job.role,
        job.subject.clone(),
        job.body.clone(),
    );
    spec.task.clone_from(&job.task);
    spec.target.clone_from(&job.target);
    spec.from_ref.clone_from(&job.from_ref);
    spec.branch.clone_from(&job.branch);
    spec.publish_ref.clone_from(&job.publish_ref);
    spec.publish_expect.clone_from(&job.publish_expect);
    spec.publish_contains.clone_from(&job.publish_contains);
    spec.delete_ref.clone_from(&job.delete_ref);
    spec.delete_expect.clone_from(&job.delete_expect);
    spec.pool_candidates.clone_from(&job.pool_candidates);
    spec.pool_exclude.clone_from(&job.pool_exclude);
    spec.extra_targets.clone_from(&job.extra_targets);
    spec.with.clone_from(&job.with);
    spec.model.clone_from(&job.model);
    spec.command.clone_from(&job.command);
    spec.builtin_role.clone_from(&job.builtin_role);
    spec.allow_default_branch = job.allow_default_branch;
    spec.local_only = job.local_only;
    spec.read_only = job.read_only;
    spec.budget = job.budget;
    spec.units.clone_from(&job.units);
    spec.source_jobs.clone_from(&job.source_jobs);
    spec.payload = job.payload.clone();
    spec
}

#[cfg(test)]
#[path = "operator_actions_test.rs"]
mod tests;
