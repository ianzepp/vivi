//! The slicer step handler: a read-only slicer role job whose reply fans out into slices.
//!
//! Node `slice` only. Node `rework1` (also class slicer) belongs to a later
//! slice and gets no handler here.

use serde_json::json;

use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs;
use crate::pump::flow::verify_inputs::{AdmittedPlan, admitted_plan};
use crate::pump::jobs::JobStore;

/// The node this handler owns.
const SLICE_NODE: &str = "slice";

/// Starts the `slice` step as a read-only slicer job over the admitted plan.
struct SliceStep;

impl StepHandler for SliceStep {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let brief = brief(cx)?;
        let mut spec = step_inputs::read_only_job(
            cx.ctx,
            cx.run,
            step,
            &NodeClass::Slicer,
            SLICER_ROLE,
            &brief.text,
        )?;
        // Frozen when queued: the slicer and its collector read this record
        // even when the plan admission moves on later.
        spec.payload = json!({
            "admitted_plan": {
                "run": brief.plan.run,
                "planner": brief.plan.planner,
                "admitter": brief.plan.admitter,
            },
        });
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// The slicer's brief text and the admitted plan it was cut from.
struct SliceBrief {
    text: String,
    plan: AdmittedPlan,
}

/// The slicer's brief: mode, ids, targets, slice size, provenance and the
/// admitted plan. Missing admission is an error: slice never restarts the
/// planner on its own.
fn brief(cx: &StartCx<'_>) -> Result<SliceBrief, VivariumError> {
    let plan = admitted_plan(cx.store, &cx.run.need, cx.run.run, cx.def)?.ok_or_else(|| {
        VivariumError::Parse(format!(
            "slice step of {}: no admitted plan to slice",
            cx.run.need
        ))
    })?;
    let text = format!(
        "mode: slice\nneed: {}\nrun: {}\ntargets: {}\nslice size: aim for 5 to {} minutes per slice\nadmitted plan source: run {} planner task {} admitter task {}\n\nadmitted plan:\n{}",
        cx.run.need,
        cx.run.run,
        step_inputs::targets(cx.ctx).join(", "),
        cx.ctx.cfg.slice.minutes(),
        plan.run,
        plan.planner,
        plan.admitter,
        plan.document,
    );
    Ok(SliceBrief { text, plan })
}

/// Registry hook: the slicer handler for node `slice` of the build flow.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.node_handler(FlowKind::Build, SLICE_NODE, Box::new(SliceStep));
}

#[cfg(test)]
#[path = "slicer_test.rs"]
mod tests;
