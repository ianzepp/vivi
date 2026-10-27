//! Park handling for the build flow: the step that holds a run for the operator.
//!
//! The router opens a park row already Waiting and records the finished step
//! whose answer routed the run here, so the tick never calls `start` for it;
//! the alarm is raised from `poll`, which runs for waiting rows (as the lock
//! step does). The step raises one alarm naming the need, the originating
//! step and its answer, and keeps waiting: a reply on the need's thread
//! continues the same step on a new thread (the `thread-change` phase), and
//! an operator answer on the step routes it.

use crate::error::VivariumError;
use crate::pump::flow::continuation::{Origin, origin_of};
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
        raise_once(cx.ctx, NEED_PARKED, &step.task, &parked_text(cx, step)?);
        Ok(None)
    }
}

/// The alarm text: the step whose answer parked the run, its word and reason,
/// and the same-step continuation a thread reply serves. A park whose origin
/// cannot be resolved stays parked with the lookup's diagnostic.
fn parked_text(cx: &StartCx<'_>, step: &StepRow) -> Result<String, VivariumError> {
    match origin_of(cx.store, cx.def, &cx.run.need, step)? {
        Origin::Found(origin) => found_text(cx, step, &origin.step),
        Origin::Parked(diagnostic) => Ok(format!(
            "need {} r{} parked at {}: {diagnostic}; \
             answer with `vivi pump answer {}`",
            cx.run.need, cx.run.run, step.node, step.task
        )),
    }
}

/// The alarm text for a resolved origin: node and task, the parked answer and
/// reason, the same-step continuation on a new thread reply.
fn found_text(cx: &StartCx<'_>, park: &StepRow, origin: &StepRow) -> Result<String, VivariumError> {
    let word = origin.verdict.as_deref().unwrap_or("none");
    let reason = parked_reason(cx, &origin.task)?;
    Ok(format!(
        "need {} r{} parked at {}: {} ({}) answered {word}: {reason}; a reply on \
         the need thread continues it on a new thread, or answer with \
         `vivi pump answer {}`",
        cx.run.need, cx.run.run, park.node, origin.node, origin.task, park.task
    ))
}

/// The reason front field of the origin's stored payload, `none given` when
/// absent.
fn parked_reason(cx: &StartCx<'_>, task: &str) -> Result<String, VivariumError> {
    let reason = match cx.store.step_payload(task)? {
        Some(row) => row.payload["front"]["reason"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        None => None,
    };
    Ok(reason.as_deref().unwrap_or("none given").to_owned())
}

/// Registry hook: the park handler.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Park, Box::new(ParkStep));
}

#[cfg(test)]
#[path = "park_test.rs"]
mod tests;
