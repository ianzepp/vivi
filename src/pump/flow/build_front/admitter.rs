//! The admitter step handler (admit1, admit2): a read-only admitter role job.

use crate::error::VivariumError;
use crate::mailspace::shipped::ADMITTER_ROLE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs;
use crate::pump::jobs::JobStore;

/// Starts an admitter step: the plan to check and the need thread.
struct AdmitterStep;

impl StepHandler for AdmitterStep {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let plan = plan_of(cx, step)?;
        let brief = format!(
            "need: {}\nrun: {}\n\nplan:\n{plan}\n\nthread:\n{}",
            cx.run.need,
            cx.run.run,
            step_inputs::need_thread(cx.ctx, &cx.run.need)?,
        );
        let spec = step_inputs::read_only_job(
            cx.ctx,
            cx.run,
            step,
            &NodeClass::Admitter,
            ADMITTER_ROLE,
            &brief,
        )?;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// The newest reply of the planner step that feeds this admitter step.
fn plan_of(cx: &StartCx<'_>, step: &StepRow) -> Result<String, VivariumError> {
    let node = &step.node;
    let planner =
        step_inputs::predecessor_step(cx.def, cx.store, cx.run, node, &NodeClass::Planner)?
            .ok_or_else(|| {
                VivariumError::Parse(format!("admitter step {node} has no finished planner step"))
            })?;
    step_inputs::step_replies(cx.mail, &planner.task)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            VivariumError::Parse(format!(
                "admitter step {node}: planner step {} has no reply",
                planner.node
            ))
        })
}

/// Register the admitter handler for the build flow.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Admitter, Box::new(AdmitterStep));
}

#[cfg(test)]
#[path = "admitter_test.rs"]
mod tests;
