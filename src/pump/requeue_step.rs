//! Operator requeue of a flow step: `vivi pump requeue <step task>`.
//!
//! The task handle is looked up in the flow run index, and the request is
//! posted as the requeue action the flow engine applies
//! ([`crate::pump::flow::operator_actions`]), with the step task as its unit,
//! on the open pump record of the step's need.

use std::collections::BTreeSet;
use std::path::Path;

use crate::VivariumError;
use crate::mailspace::{Mailspace, PumpActionRequest, PumpState};
use crate::pump::flow::index::run_state;
use crate::pump::store::PumpStore;

/// A step task and the need whose run holds it.
pub(super) type StepTarget = (String, String);

/// Each of `tasks` with the need of the active run that holds it. A task that
/// is not a step of an active run is refused, naming it.
///
/// # Errors
/// Returns [`VivariumError::Message`] for a mailspace with no pump database or
/// a task that is not a step of an active run, and whatever reading the run
/// index fails with.
pub(super) fn resolve(db: &Path, tasks: &[String]) -> Result<Vec<StepTarget>, VivariumError> {
    let Some(store) = PumpStore::open_read_only(db)? else {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            db.display()
        )));
    };
    let mut found = Vec::new();
    for task in tasks {
        let Some(need) = active_need(&store, task)? else {
            return Err(VivariumError::Message(format!(
                "{task} is not a step of an active flow run (see `vivi flow show <kind> --need <need>`)"
            )));
        };
        found.push((task.clone(), need));
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
        .filter(|record| record.letter_error.is_none())
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
        let request = PumpActionRequest::requeue(task, None, false, false, reason);
        mailspace.pump_post_action(None, need, &request)?;
        println!("{}", super::requested_line(task));
    }
    Ok(())
}
