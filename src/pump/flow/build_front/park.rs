//! Park handling for the build flow: the step that holds a run for the mind.
//!
//! The router opens a park row already Waiting, so the tick never calls
//! `start` for it; the alarm is raised from `poll`, which runs for waiting
//! rows (as the lock step does). The step raises one alarm naming the need and
//! why the planner parked it, and keeps waiting: resuming a parked run at
//! `plan` is a mind-owned decision, taken by an explicit
//! `vivi pump replan <need>`; an operator answer on this step is not the
//! resume route.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs;
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
            "need {} r{} parked at {}: expected the planner to answer planned; actual: {actual}; \
             the mind owns this decision: resume at plan with `vivi pump replan {}`",
            cx.run.need, cx.run.run, step.node, cx.run.need
        );
        raise_once(cx.ctx, NEED_PARKED, &step.task, &text);
        Ok(None)
    }
}

/// The planner word and reason that routed the run here.
fn actual_text(cx: &StartCx<'_>, step: &StepRow) -> Result<String, VivariumError> {
    let Some(planner) =
        step_inputs::predecessor_step(cx.def, cx.store, cx.run, &step.node, &NodeClass::Planner)?
    else {
        return Ok("no planner step found".into());
    };
    let word = planner.verdict.as_deref().unwrap_or("none");
    let reason = match cx.store.step_payload(&planner.task)? {
        Some(row) => row.payload["front"]["reason"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        None => None,
    };
    Ok(format!(
        "{word}: {}",
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
