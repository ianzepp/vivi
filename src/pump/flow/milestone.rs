//! Release milestone handler: the `wait` node answers `ready` once no need
//! prerequisite of the run's need is left in the backlog graph.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::jobs::JobStore;
use crate::pump::ready::blocked_needs;

mod prereq;

/// The word the milestone answers when every prerequisite need is done.
const READY: &str = "ready";

/// Waits for the need's prerequisite needs; `poll` answers when none is left.
struct Milestone;

impl StepHandler for Milestone {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }

    fn poll(&self, cx: &StartCx<'_>, _step: &StepRow) -> Result<Option<String>, VivariumError> {
        // An unreadable graph keeps the milestone waiting.
        let Some(blocked) = blocked_needs(cx.ctx) else {
            return Ok(None);
        };
        Ok(if blocked.contains_key(&cx.run.need) {
            None
        } else {
            Some(READY.to_owned())
        })
    }
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Release, NodeClass::Milestone, Box::new(Milestone));
}

#[cfg(test)]
#[path = "milestone_test.rs"]
mod tests;
