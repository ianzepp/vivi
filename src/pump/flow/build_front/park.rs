//! Park handling for the build flow: the step that holds a run for the operator.
//!
//! The router opens a park row already Waiting, so the tick never calls
//! `start` for it; the alarm is raised from `poll`, which runs for waiting
//! rows (as the lock step does). The step raises one alarm naming the need,
//! the executable task whose answer parked the run, and that task's verdict
//! and reason, and keeps waiting: a reply on the need's thread supplies new
//! input to that same stopped step, and an operator answer on the step
//! routes it.

use crate::error::VivariumError;
use crate::pump::flow::continuation;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::tick::raise_once;
use crate::pump::jobs::JobStore;

/// Exception kind raised once when a run parks.
pub const NEED_PARKED: &str = "flow-need-parked";

/// Opens the park step's task and waits; `poll` raises the alarm once.
struct ParkStep;

impl StepHandler for ParkStep {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }

    fn poll(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<Option<String>, VivariumError> {
        let actual = actual_text(cx, step)?;
        let text = format!(
            "need {} r{} parked at {}: actual: {actual}; a reply on the need thread \
             supplies new input to that same step, or answer with `vivi pump answer {}`",
            cx.run.need, cx.run.run, step.node, step.task
        );
        raise_once(cx.ctx, NEED_PARKED, &step.task, &text);
        Ok(None)
    }
}

/// The stopped source, from the shared origin resolver: the executable task
/// whose verdict routed the run to the park, with its answer word and the
/// reason recorded at it. A park opened before origins were linked resolves
/// from the run's completed steps and the flow topology; missing or
/// ambiguous evidence degrades the alarm text instead of failing the park.
fn actual_text(cx: &StartCx<'_>, step: &StepRow) -> Result<String, VivariumError> {
    let origin = match continuation::origin(cx.def, cx.store, cx.run, step) {
        Ok(origin) => origin,
        Err(error) => return Ok(format!("the stopped step is unresolved: {error}")),
    };
    let word = cx
        .store
        .step_by_task(&origin)?
        .and_then(|row| row.verdict)
        .unwrap_or_else(|| "none".into());
    let reason = match cx.store.step_payload(&origin)? {
        Some(row) => row.payload["front"]["reason"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        None => None,
    };
    Ok(format!(
        "{origin} answered {word}: {}",
        reason.as_deref().unwrap_or("none given")
    ))
}

/// Registry hook: the park handler.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Park, Box::new(ParkStep));
}

#[cfg(test)]
#[path = "park_test.rs"]
mod tests;
