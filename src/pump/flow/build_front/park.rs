//! Park handling for the build flow: the step that holds a run for the operator.
//!
//! The step opens its task and waits. Nothing polls it: a reply on the need's
//! thread restarts a parked run at `plan` (the `thread-change` phase), and an
//! operator answer on the step routes it.

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::jobs::JobStore;

/// Opens the park step's task and waits for a restart or an answer.
struct ParkStep;

impl StepHandler for ParkStep {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }
}

/// Registry hook: the park handler.
pub(super) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::Park, Box::new(ParkStep));
}
