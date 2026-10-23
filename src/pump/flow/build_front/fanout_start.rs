//! Fanout start: expands the slicer's slices once the slicer step is done.
//!
//! The slices come from the slicer's reply on the slicer step task (never
//! from pump.db), newest reply first, so a later correction wins.

use crate::error::VivariumError;
use crate::pump::envelope::roles::{Slice, SlicerAnswer};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fanout::{SLICES_INVALID, check_batch, parse_reply};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs;
use crate::pump::jobs::JobStore;

/// Starts a fanout step from the slicer's reply.
struct FanoutStart;

/// Registry hook: the fanout handler of both build passes.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Fanout, Box::new(FanoutStart));
}

impl StepHandler for FanoutStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let slicer = step_inputs::predecessor_step(
            cx.def,
            cx.store,
            cx.run,
            &step.node,
            &NodeClass::Slicer,
        )?
        .ok_or_else(|| {
            VivariumError::Other(format!("fanout {} has no done slicer step", step.node))
        })?;
        let slices = match latest_slices(cx, &slicer.task)? {
            Ok(slices) => slices,
            Err(reason) => return Ok(invalid(cx, step, &reason)),
        };
        let problems = problems(cx, &slices);
        if !problems.is_empty() {
            return Ok(invalid(cx, step, &problems.join("; ")));
        }
        cx.store
            .set_open_fanout(&cx.run.need, cx.run.run, &step.task)?;
        Ok(StartAction::Fanout {
            slicer_step: slicer,
            slices,
        })
    }
}

/// The slices of the newest reply on `task` that parses as a sliced envelope,
/// or the reason there is none.
fn latest_slices(
    cx: &StartCx<'_>,
    task: &str,
) -> Result<Result<Vec<Slice>, String>, VivariumError> {
    let replies = step_inputs::step_replies(cx.mail, task)?;
    if replies.is_empty() {
        return Ok(Err(format!("slicer task {task} has no pool result reply")));
    }
    for reply in &replies {
        if let Ok(envelope) = parse_reply(reply)
            && envelope.answer == SlicerAnswer::Sliced
        {
            return Ok(Ok(envelope.slice));
        }
    }
    Ok(Err(format!(
        "no reply on slicer task {task} is a valid sliced envelope"
    )))
}

/// What is wrong with `slices`: unknown targets, empty write scopes, and a
/// duplicate id, unknown dependency or cycle.
fn problems(cx: &StartCx<'_>, slices: &[Slice]) -> Vec<String> {
    let known = step_inputs::targets(cx.ctx);
    let mut out = Vec::new();
    for slice in slices {
        if !known.contains(&slice.target) {
            out.push(format!(
                "slice {} names unknown target {}",
                slice.id, slice.target
            ));
        }
        if slice.write_scope.iter().all(|path| path.trim().is_empty()) {
            out.push(format!("slice {} has an empty write_scope", slice.id));
        }
    }
    if let Err(reason) = check_batch(cx.store, cx.run, slices) {
        out.push(reason);
    }
    out
}

/// Raise `flow-slices-invalid` and answer failed so routing goes to the CTO.
fn invalid(cx: &StartCx<'_>, step: &StepRow, text: &str) -> StartAction {
    cx.ctx.exception(SLICES_INVALID, &step.task, text);
    StartAction::Decide {
        word: "failed".into(),
    }
}

#[cfg(test)]
#[path = "fanout_start_test.rs"]
mod tests;
