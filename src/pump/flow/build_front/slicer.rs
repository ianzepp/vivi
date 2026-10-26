//! The slicer step handler: a read-only slicer role job whose reply fans out into slices.
//!
//! Node `slice` only. Node `rework1` (also class slicer) belongs to a later
//! slice and gets no handler here.

use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs;
use crate::pump::flow::verify_inputs;
use crate::pump::jobs::JobStore;

/// The node this handler owns.
const SLICE_NODE: &str = "slice";

/// Starts the `slice` step as a read-only slicer job over the admitted plan.
struct SliceStep;

impl StepHandler for SliceStep {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let brief = brief(cx)?;
        let spec = step_inputs::read_only_job(
            cx.ctx,
            cx.run,
            step,
            &NodeClass::Slicer,
            SLICER_ROLE,
            &brief,
        )?;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// The slicer's brief: mode, ids, targets, slice size and the admitted plan.
fn brief(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let plan = admitted_plan(cx)?;
    Ok(format!(
        "mode: slice\nneed: {}\nrun: {}\ntargets: {}\nslice size: aim for 5 to {} minutes per slice\n\nadmitted plan:\n{plan}",
        cx.run.need,
        cx.run.run,
        step_inputs::targets(cx.ctx).join(", "),
        cx.ctx.cfg.slice.minutes(),
    ))
}

/// The admitted plan the run continues, or an error when the run has none:
/// the slicer rejects missing input instead of slicing without a plan.
fn admitted_plan(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    match verify_inputs::admitted_plan(cx.store, cx.run)? {
        Some(plan) => Ok(plan),
        None => Err(VivariumError::Parse(format!(
            "slice step of {} run {}: no admitted plan to slice",
            cx.run.need, cx.run.run
        ))),
    }
}

/// Registry hook: the slicer handler for node `slice` of the build flow.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.node_handler(FlowKind::Build, SLICE_NODE, Box::new(SliceStep));
}

#[cfg(test)]
#[path = "slicer_test.rs"]
mod tests;
