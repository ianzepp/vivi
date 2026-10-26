//! Operator replan, withdraw and requeue letters: the flow owns every one.
//!
//! This tick phase reads the action letters the flow has not applied. For a
//! need with an active run (`active_run`):
//!
//! - `replan` restarts the live run at `plan`;
//! - `withdraw` closes the need's pump record (the tick withdraws a run whose
//!   record closed);
//! - `requeue` queues the step's newest execution again, as `<job id>-q<n>`,
//!   at the same attempt; it is refused unless that job is terminal and the
//!   step is open (a done step is reopened by advance, never here). The copy
//!   names the superseded execution at a raised replacement order and mints a
//!   genuinely new transient input generation, and any retry dispatch hold
//!   the superseded execution carried is shed. A Pending job held after
//!   repeated pool refusals is released in place instead: its hold and
//!   refusal count clear and it takes the same replacement and generation
//!   stamps, so the next dispatch offers a fresh execution to a pool again.
//!   The fresh generation is a digest of the action's durable id, so recovery
//!   of the same action cannot reset the retry budget twice.
//!
//! For a need with no active run (none, or a done, locked or withdrawn one):
//!
//! - `withdraw` closes the need's open pump record `withdrawn`;
//! - `requeue` and `replan` are refused: there is no run to act on, and a
//!   locked run is opened with `vivi pump unlock <need>`.
//!
//! `answer` and `unlock` are left alone: other phases apply them.
//!
//! Every other letter is applied or refused exactly once: its id goes into
//! `applied_actions` either way, and a refusal raises `flow-action-refused`.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::mailspace::{PumpAction, PumpActionKind};
use crate::pump::flow::advance::restart::{self as run_restart};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{RunRow, StepJobRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobStatus, JobStore, PoolJob, replay};
use crate::pump::store::PumpStore;

/// Exception kind of an operator action the flow cannot apply.
pub const ACTION_REFUSED: &str = "flow-action-refused";

/// The flow node a replan restarts at.
const REPLAN_ENTRY: &str = "plan";

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
            if matches!(
                action.request.kind,
                PumpActionKind::Answer | PumpActionKind::Unlock
            ) {
                continue;
            }
            let result = match active_run(store, &action.need)? {
                Some(run) => apply_one(ctx, st, store, mail, &run, &action),
                None => apply_without_run(mail, &action),
            };
            match result {
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

/// The alarm text of a refused action.
fn refusal(action: &PumpAction, error: &VivariumError) -> String {
    format!(
        "{} action {} on need {}: {error}",
        action.request.kind.as_str(),
        action.id,
        action.need
    )
}

/// Apply one action to a need with no active run: a withdraw closes its
/// record, everything else is refused.
fn apply_without_run<M: MailWriter>(
    mail: &mut M,
    action: &PumpAction,
) -> Result<(), VivariumError> {
    match action.request.kind {
        PumpActionKind::Withdraw => mail.close_record(&action.need, "withdrawn"),
        _ => Err(VivariumError::Message(
            "no active flow run (use `vivi pump unlock <need>` for a locked run)".into(),
        )),
    }
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
        PumpActionKind::Requeue => requeue(st, store, run, action),
        PumpActionKind::Answer | PumpActionKind::Unlock => Err(VivariumError::Message(format!(
            "{} is applied by another phase",
            action.request.kind.as_str()
        ))),
    }
}

/// Queue the step's newest execution again at the same attempt. The copy is
/// the shared replay copy: it carries every execution input, names the
/// superseded execution at a raised replacement order, mints the action's
/// fresh input generation, and sheds the superseded execution's retry
/// dispatch hold — an explicit requeue dispatches now.
fn requeue(
    st: &mut impl JobStore,
    store: &PumpStore,
    run: &RunRow,
    action: &PumpAction,
) -> Result<(), VivariumError> {
    let task = action
        .request
        .unit
        .as_deref()
        .ok_or_else(|| VivariumError::Message("requeue names no step task".into()))?;
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
    if release_held(st, store, &run.need, task, &action.id)? {
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
    let mut spec = replay::copy_spec(&old, id);
    replay::stamp_replacement(&mut spec.payload, &old);
    restamp_fresh_execution(&mut spec.payload, &action.id, &old.payload);
    again.stamp(&run.need, &mut spec.payload);
    jobs::enqueue(st, spec)?;
    store.insert_step_job(&run.need, &again)?;
    store.set_step_state(&step.task, StepState::Running, None)
}

/// Stamp `payload` as a genuinely new execution of the requeued step: a new
/// transient input generation derived from the durable action identity, and
/// no dispatch hold — a retry deadline the superseded execution still
/// carried is obsolete once the operator requeues by hand.
fn restamp_fresh_execution(
    payload: &mut serde_json::Value,
    action_id: &str,
    previous: &serde_json::Value,
) {
    replay::replace_input(
        payload,
        action_generation(action_id, replay::input_generation(previous)),
    );
    replay::stamp_dispatch_not_before(payload, 0);
}

/// The fresh transient input generation an explicit requeue mints: a digest
/// of the durable action identity, so recovery of the same action derives
/// the same generation and cannot reset the retry budget twice. `previous`
/// is the superseded execution's generation; a digest colliding with it is
/// bumped past, so the generation is genuinely new. The retry store counts
/// generations as `u32`, so the mint stays in that universe.
fn action_generation(action_id: &str, previous: u64) -> u64 {
    const DOMAIN: &[u8] = b"vivi:requeue-input-generation";
    const MAX: u64 = u32::MAX as u64;
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update([0]);
    hasher.update(action_id.as_bytes());
    let digest = hasher.finalize();
    let mut head = [0u8; 4];
    let len = digest.as_slice().len().min(4);
    head[..len].copy_from_slice(&digest.as_slice()[..len]);
    let fresh = u64::from(u32::from_be_bytes(head)).max(1);
    if fresh == previous {
        if previous >= MAX { 1 } else { previous + 1 }
    } else {
        fresh
    }
}

/// Release every held Pending job of step `task` in place: a job whose
/// payload names this task and need (a slice-merge job, a correction) or that
/// has a `step_jobs` row for it. The release is an explicit requeue of the
/// current execution, so each released job is restamped a replacement of
/// itself at a raised order, mints the action's fresh input generation, and
/// sheds its retry dispatch hold. A pending job already restamped in place
/// counts as handled, so recovery of the same action is a no-op. True when
/// any job was released or already stood released.
fn release_held(
    st: &mut impl JobStore,
    store: &PumpStore,
    need: &str,
    task: &str,
    action_id: &str,
) -> Result<bool, VivariumError> {
    let mut ids = Vec::new();
    for job in st.jobs().values() {
        if job.status != JobStatus::Pending {
            continue;
        }
        let stamped = job.payload["task"] == task && job.payload["need"] == need;
        if !(stamped || store.step_job(&job.id)?.is_some_and(|row| row.task == task)) {
            continue;
        }
        if job.held.is_some() || restamped_in_place(job) {
            ids.push(job.id.clone());
        }
    }
    for id in &ids {
        if let Some(job) = st.jobs_mut().get_mut(id) {
            if job.held.is_none() {
                continue; // released by an earlier application of an action
            }
            let snapshot = job.clone();
            replay::stamp_replacement(&mut job.payload, &snapshot);
            restamp_fresh_execution(&mut job.payload, action_id, &snapshot.payload);
            job.held = None;
            job.dispatch_refusals = 0;
        }
    }
    Ok(!ids.is_empty())
}

/// Whether a pending job was already requeued in place: it names itself as
/// its superseded execution, the stamp no other write makes.
fn restamped_in_place(job: &PoolJob) -> bool {
    replay::replacement_predecessor(&job.payload) == Some(job.id.as_str())
}

/// The step's newest execution: highest attempt, then highest recorded
/// replacement order, then latest queue second — never the lexical job id,
/// so a late original or correction result is not picked over the
/// replacement that superseded it. Historical jobs without replacement
/// stamps keep their old queued-at ordering.
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
        let newer = best.as_ref().is_none_or(|(held, kept)| {
            (row.attempt, replacement_rank(job)) > (kept.attempt, replacement_rank(held))
        });
        if row.task == task && newer {
            best = Some((job.clone(), row));
        }
    }
    Ok(best)
}

/// The recorded replacement ordering of a job: its payload replacement
/// order, then the queue second for historical jobs that carry no stamp.
fn replacement_rank(job: &PoolJob) -> (u64, u64) {
    (replay::replacement_order(&job.payload), job.queued_at)
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

#[cfg(test)]
#[path = "operator_actions_test.rs"]
mod tests;
