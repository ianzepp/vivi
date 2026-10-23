//! Park handling for the build flow: the step that holds a run for the operator.
//!
//! The step opens its task, raises one alarm naming the need and why the
//! planner parked it, and waits. Nothing polls it: a reply on the need's
//! thread restarts a parked run at `plan` (the `thread-change` phase), and an
//! operator answer on the step routes it.

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

/// Opens the park step's task, raises the alarm, and waits for a restart or an answer.
struct ParkStep;

impl StepHandler for ParkStep {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let actual = actual_text(cx, step)?;
        let text = format!(
            "need {} r{} parked at {}: expected the planner to answer planned; actual: {actual}; \
             a reply on the need thread restarts it at plan, or answer with `vivi pump answer {}`",
            cx.run.need, cx.run.run, step.node, step.task
        );
        raise_once(cx.ctx, NEED_PARKED, &step.task, &text);
        Ok(StartAction::OpenAndWait)
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
