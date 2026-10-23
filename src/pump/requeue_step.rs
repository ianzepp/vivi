//! Operator requeue of a flow step: `vivi pump requeue <step task>`.
//!
//! A flow step is not a unit of the old pipeline, so the unit path of
//! [`super::run`] does not know it. The task handle is looked up in the flow
//! run index instead, and the request is posted as the same requeue action
//! the flow engine applies ([`crate::pump::flow::operator_actions`]), with
//! the step task as its unit, on the open pump record of the step's need.

use std::collections::BTreeSet;
use std::path::Path;

use super::AttemptPolicy;
use crate::VivariumError;
use crate::mailspace::{Mailspace, PumpActionRequest, PumpState};
use crate::pump::flow::index::run_state;
use crate::pump::store::PumpStore;

/// A step task and the need whose run holds it.
pub(super) type StepTarget = (String, String);

/// The step tasks among `uids`: those that are not units but name a step of
/// an active run, each with the need of that run. A uid that is neither a
/// unit nor such a step is refused as an unknown unit.
///
/// # Errors
/// Returns [`VivariumError::Message`] for a uid that is neither a unit nor a
/// step, and whatever reading the run index fails with.
pub(super) fn resolve(
    db: &Path,
    uids: &[String],
    is_unit: impl Fn(&str) -> bool,
) -> Result<Vec<StepTarget>, VivariumError> {
    let mut found = Vec::new();
    let mut store = None;
    for uid in uids.iter().filter(|uid| !is_unit(uid)) {
        if store.is_none() {
            store = PumpStore::open_read_only(db)?;
        }
        let need = match store.as_ref() {
            Some(open) => active_need(open, uid)?,
            None => None,
        };
        let Some(need) = need else {
            return Err(VivariumError::Message(format!("unknown unit: {uid}")));
        };
        found.push((uid.clone(), need));
    }
    Ok(found)
}

/// The need of the active run holding step task `task`, if there is one.
fn active_need(store: &PumpStore, task: &str) -> Result<Option<String>, VivariumError> {
    let Some(step) = store.step_by_task(task)? else {
        return Ok(None);
    };
    let active = [
        run_state::STARTING,
        run_state::LIVE,
        run_state::RESTARTING,
        run_state::FINISHING,
    ];
    for state in active {
        for run in store.runs_in_state(state)? {
            if run.run == step.run
                && store
                    .steps_of(&run.need, run.run)?
                    .iter()
                    .any(|row| row.task == task)
            {
                return Ok(Some(run.need));
            }
        }
    }
    Ok(None)
}

/// Check that every step's need has an open pump record, then post one
/// requeue action per step, or only print what would be posted when
/// `dry_run`. Nothing is posted when any need has no open record.
///
/// # Errors
/// Returns [`VivariumError::Message`] for a step whose need has no open pump
/// record, and whatever reading records or posting an action fails with.
pub(super) fn post(
    project: &Path,
    steps: &[StepTarget],
    policy: AttemptPolicy,
    reason: &str,
    dry_run: bool,
) -> Result<(), VivariumError> {
    if steps.is_empty() {
        return Ok(());
    }
    let mailspace = Mailspace::discover(Some(project))?;
    let open: BTreeSet<String> = mailspace
        .pump_list(None, Some(PumpState::Open))?
        .into_iter()
        .map(|record| record.need)
        .collect();
    for (task, need) in steps {
        if !open.contains(need) {
            return Err(VivariumError::Message(format!(
                "step {task} belongs to need {need}, which has no open pump record, so there is no dispatch to requeue it in; open one with `vivi pump send --need {need}`"
            )));
        }
    }
    for (task, need) in steps {
        if dry_run {
            println!("would requeue flow step {task} of need {need}");
            continue;
        }
        let request =
            PumpActionRequest::requeue(task, policy.attempts, policy.fresh, policy.refund, reason);
        mailspace.pump_post_action(None, need, &request)?;
        println!("{}", super::requested_line(task));
    }
    Ok(())
}
