//! Follow-up units: a rework-mode slicer job for work found after a slice is built.
//!
//! Each required follow-up of a built slice goes to the slicer in rework mode,
//! which chooses the owner. Optional follow-ups are only posted on the slice
//! task (the default built-slice hook does that); this hook only routes.

use crate::error::VivariumError;
use crate::pump::envelope::roles::{FollowUp, Hand};
use crate::pump::flow::collect_slice::BuiltHook;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::reslice::{self, Mode};
use crate::pump::jobs::JobStore;

/// Sends one slicer rework job per required follow-up of a built slice.
pub(crate) struct FollowUpRework;

/// Register the follow-up routing hook after the default notes hook.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.built_hook(Box::new(FollowUpRework));
}

impl<S: JobStore, M: FlowMail> BuiltHook<S, M> for FollowUpRework {
    fn built(
        &self,
        cx: &mut RunCx<'_, S, M>,
        step: &StepRow,
        envelope: &Hand,
    ) -> Result<(), VivariumError> {
        for item in envelope.follow_up.iter().filter(|item| !item.optional) {
            let input = request_text(step, item);
            reslice::request(
                cx.ctx,
                cx.st,
                cx.store,
                cx.mail,
                &cx.run,
                step,
                Mode::FollowUp,
                &input,
            )?;
        }
        Ok(())
    }
}

/// The ask the slicer receives for one required follow-up.
fn request_text(step: &StepRow, item: &FollowUp) -> String {
    format!(
        "follow-up from slice {} ({}):\npaths: {}\nneed: {}\n\
         Answer sliced to keep this work in this need, escalate to file it as a new need.\n",
        step.node,
        step.task,
        item.paths.join(", "),
        item.need
    )
}

#[cfg(test)]
#[path = "follow_up_test.rs"]
mod tests;
