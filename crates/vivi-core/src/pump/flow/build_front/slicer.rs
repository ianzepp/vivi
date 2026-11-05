//! The slicer step handler: a read-only slicer role job whose reply fans out into slices.
//!
//! Node `slice` only. Node `rework1` (also class slicer) belongs to a later
//! slice and gets no handler here.

use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::envelope::format;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs;
use crate::pump::jobs::JobStore;
use vivi_error::VivariumError;
use vivi_mail::letter::roles::SlicerMode;

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
    let targets = step_inputs::targets(cx.ctx);
    let target = targets.first().ok_or_else(|| {
        VivariumError::Config(String::from("slice brief has no configured target"))
    })?;
    let mut text = format!(
        "mode: slice\nneed: {}\nrun: {}\ntargets: {}\nslice size: aim for 5 to {} minutes per slice\n\nadmitted plan:\n{plan}",
        cx.run.need,
        cx.run.run,
        targets.join(", "),
        cx.ctx.cfg.slice.minutes(),
    );
    text.push_str("\n\n");
    text.push_str(&format::slicer(
        SlicerMode::Slice,
        target,
        &format!("slice-{}-example", cx.run.need),
    ));
    Ok(text)
}

/// The newest reply of the newest Done planner step: `plan2` when a run
/// revised its plan, else `plan`. This run's own plan wins; a run that
/// started at `slice` (a head-cto restart) has none, so it slices the plan of
/// the newest earlier run of the need that has one.
fn admitted_plan(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let need = &cx.run.need;
    let planner = newest_planner_step(cx, need)?.ok_or_else(|| {
        VivariumError::Parse(format!(
            "slice step of {need}: no done planner step to slice"
        ))
    })?;
    step_inputs::step_replies(cx.mail, &planner.task)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            VivariumError::Parse(format!(
                "slice step of {need}: planner step {} has no reply",
                planner.node
            ))
        })
}

/// The newest Done `plan2`, else `plan`, row of this run or, failing that, of
/// the nearest earlier run.
fn newest_planner_step(cx: &StartCx<'_>, need: &str) -> Result<Option<StepRow>, VivariumError> {
    for run in (1..=cx.run.run).rev() {
        let rows = cx.store.steps_of(need, run)?;
        for node in ["plan2", "plan"] {
            let done = rows
                .iter()
                .rfind(|row| row.node == node && row.state == StepState::Done);
            if let Some(row) = done {
                return Ok(Some(row.clone()));
            }
        }
    }
    Ok(None)
}

/// Registry hook: the slicer handler for node `slice` of the build flow.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.node_handler(FlowKind::Build, SLICE_NODE, Box::new(SliceStep));
}

#[cfg(test)]
#[path = "slicer_test.rs"]
mod tests;
