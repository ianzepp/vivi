//! Handler for the judge steps. The judge is the auditor role in judge mode: it
//! reads the audit and the need rung report and rules `block` or `residual`.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::step_inputs::make_read_only;
use crate::pump::flow::verify_inputs::{capped, latest_of_class, rung_report};
use crate::pump::jobs::JobStore;

/// The cap on the audit text a judge brief carries.
const AUDIT_CAP: usize = 30_000;
const AUDITOR_ROLE: &str = "auditor";
const AUDITOR_CLASS: &str = "auditor";
const RUNG_CLASS: &str = "rung-need";

/// Starts a judge step as a read-only auditor job.
pub(crate) struct JudgeStart;

impl StepHandler for JudgeStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let need = &cx.run.need;
        let rows = cx.store.step_payloads_of_run(need, cx.run.run)?;
        let audit = latest_of_class(&rows, AUDITOR_CLASS).ok_or_else(|| {
            VivariumError::Message(format!(
                "judge for need {need} has no audit row in this run"
            ))
        })?;
        let rung = latest_of_class(&rows, RUNG_CLASS).ok_or_else(|| {
            VivariumError::Message(format!(
                "judge for need {need} has no need rung row in this run"
            ))
        })?;
        let audit_text = format!(
            "front:\n{}\n\nbody:\n{}",
            audit.payload["front"],
            audit.payload["body"].as_str().unwrap_or("")
        );
        let brief = format!(
            "Judge mode\nneed: {need}\n\n## Audit\n{}\n\n## Need rung report\n{}\n",
            capped(&audit_text, AUDIT_CAP),
            rung_report(rung)
        );
        let mut spec = role_job(cx.ctx, cx.run, step, "flow-judge", AUDITOR_ROLE, &brief);
        make_read_only(cx.ctx, &mut spec);
        spec.builtin_role = Some(AUDITOR_ROLE.to_owned());
        Ok(StartAction::Job(Box::new(spec)))
    }
}

pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Judge, Box::new(JudgeStart));
}

#[cfg(test)]
#[path = "judge_step_test.rs"]
mod tests;
