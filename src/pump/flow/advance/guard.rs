//! The completion guard over a run's tasks: the RC3 backstop.
//!
//! The run sentinel keeps a need open, but an operator, a Mind or
//! `graph audit --repair` can still close a step or the sentinel by hand.
//! [`pass`] finds those closes, reopens the task and reports it.

use crate::error::VivariumError;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskState, TaskStatus};
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

/// Exception kind raised for a task closed by someone other than the pump.
pub(crate) const FOREIGN_CLOSE: &str = "flow-foreign-close";

/// Whether the index still treats the step as running its course.
fn is_open(step: &StepRow) -> bool {
    matches!(
        step.state,
        StepState::Pending | StepState::Running | StepState::Waiting
    )
}

/// A close the pump made, or one whose verdict the index already holds.
fn pump_owned(pump: &str, step: &StepRow, mail: &TaskState) -> bool {
    if mail.verdict.is_none() {
        return false;
    }
    mail.closer.as_deref() == Some(pump) || step.verdict == mail.verdict
}

/// Reopen and report every open step or sentinel of `run` that was closed
/// by someone other than the pump (`pump` is the pump's mailspace identity).
///
/// A closed need is a withdrawal, not a foreign close, and nothing is
/// reopened under it. The exception is a need that a join closed because the
/// foreign close left every unit done: such a need has no open unit, and the
/// reopen restores it. A closer unknown because the close arrived as a
/// synced letter is reported as `unknown`.
///
/// # Errors
/// Returns [`VivariumError`] when the index or the mailspace cannot be read
/// or a reopen fails.
pub(crate) fn pass<M>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    pump: &str,
) -> Result<(), VivariumError>
where
    M: MailWriter + MailReader,
{
    let mut seen = Vec::new();
    for step in store.steps_of(&run.need, run.run)? {
        if is_open(&step) {
            let state = mail.task_state(&step.task)?;
            seen.push((step, state));
        }
    }
    let need_closed = mail.need_state(&run.need)? == NeedStatus::Done;
    let direct_close = seen.iter().any(|(_, s)| s.status == TaskStatus::Open);
    if need_closed && direct_close {
        return Ok(());
    }
    for (step, state) in &seen {
        if state.status != TaskStatus::Done || pump_owned(pump, step, state) {
            continue;
        }
        mail.reopen_task(&step.task)?;
        let closer = state.closer.as_deref().unwrap_or("unknown");
        ctx.exception(
            FOREIGN_CLOSE,
            &step.task,
            &format!("{} closed by {closer}; reopened", step.node),
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "guard_test.rs"]
mod tests;
