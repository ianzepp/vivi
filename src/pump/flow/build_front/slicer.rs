//! The slicer step handler: a read-only slicer role job whose reply fans out into slices.
//!
//! Node `slice` only. Node `rework1` (also class slicer) belongs to a later
//! slice and gets no handler here.

use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::need_scope::{TargetScope, effective_scopes};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs;
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

/// The slicer's brief: mode, ids, targets, slice size, the effective need
/// write scope and the admitted plan.
fn brief(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let plan = admitted_plan(cx)?;
    let mut text = format!(
        "mode: slice\nneed: {}\nrun: {}\ntargets: {}\nslice size: aim for 5 to {} minutes per slice\n",
        cx.run.need,
        cx.run.run,
        step_inputs::targets(cx.ctx).join(", "),
        cx.ctx.cfg.slice.minutes(),
    );
    text.push_str(&scope_block(cx, &cx.run.need)?);
    text.push_str(&format!("\nadmitted plan:\n{plan}"));
    Ok(text)
}

/// The authoritative effective need write scope: the original letter's
/// delegation plus every later pump grant, grouped by target, kept outside
/// the admitted plan. A target left out holds no delegated paths, and an
/// empty scope is not unrestricted permission.
fn scope_block(cx: &StartCx<'_>, need: &str) -> Result<String, VivariumError> {
    let groups = effective_scopes(
        cx.mail,
        need,
        &cx.ctx.cfg.reply_from,
        &step_inputs::targets(cx.ctx),
    )?;
    let mut text = String::from(
        "effective need write scope (grouped by target; the original letter's delegation plus every later pump grant; authoritative for this job and supersedes the original letter's write_scope):\n",
    );
    if groups.is_empty() {
        text.push_str(
            "- (none): the need delegates no write scope; that is not unrestricted permission\n",
        );
        return Ok(text);
    }
    for group in &groups {
        text.push_str(&target_line(group));
    }
    text.push_str("a target absent above holds no delegated paths\n");
    Ok(text)
}

/// One grouped target line: delegated paths first, then later grants.
fn target_line(group: &TargetScope) -> String {
    let mut line = format!("- target {}: ", group.target);
    if !group.original.is_empty() {
        line.push_str(&format!("delegated {}", group.original.join(", ")));
    }
    if !group.granted.is_empty() {
        if !group.original.is_empty() {
            line.push_str("; ");
        }
        line.push_str(&format!("granted {}", group.granted.join(", ")));
    }
    line.push('\n');
    line
}

/// The newest reply of the newest Done planner step: `plan2` when the run
/// revised its plan, else `plan`.
fn admitted_plan(cx: &StartCx<'_>) -> Result<String, VivariumError> {
    let need = &cx.run.need;
    let planner = match step_inputs::done_step(cx.store, cx.run, "plan2")? {
        Some(row) => Some(row),
        None => step_inputs::done_step(cx.store, cx.run, "plan")?,
    }
    .ok_or_else(|| {
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

/// Registry hook: the slicer handler for node `slice` of the build flow.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.node_handler(FlowKind::Build, SLICE_NODE, Box::new(SliceStep));
}

#[cfg(test)]
#[path = "slicer_test.rs"]
mod tests;
