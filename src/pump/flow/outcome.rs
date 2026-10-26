//! A step's outcome and the contract of the handlers that start each class of step.

use crate::error::VivariumError;
use crate::pump::envelope::roles::{RoleKind, Slice};
use crate::pump::flow::def::FlowDef;
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::host::Ctx;
use crate::pump::jobs::JobSpec;
use crate::pump::store::PumpStore;

/// What a finished step answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub need: String,
    pub run: u32,
    pub node: String,
    pub task: String,
    pub word: String,
    pub payload: serde_json::Value,
}

/// How a step begins.
#[derive(Debug, Clone, PartialEq)]
pub enum StartAction {
    /// Enqueue this pool job.
    Job(Box<JobSpec>),
    /// Open the step task and wait for someone to close it.
    OpenAndWait,
    /// Code decides at once with this word.
    Decide { word: String },
    /// Code answers at once with this word and payload.
    Answer {
        word: String,
        payload: serde_json::Value,
    },
    /// Expand the slicer's slices as slice tasks; the fanout step runs until
    /// they all settle.
    Fanout {
        slicer_step: StepRow,
        slices: Vec<Slice>,
    },
}

/// Read access a handler gets when it starts or polls a step.
pub struct StartCx<'a> {
    pub ctx: &'a Ctx,
    pub store: &'a PumpStore,
    pub mail: &'a dyn SliceMail,
    pub run: &'a RunRow,
    pub def: &'a FlowDef,
}

/// Starts one class of step.
pub trait StepHandler {
    /// How `step` begins.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the step cannot be started.
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError>;

    /// A waiting step's answer word, once its handler can give one.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the step cannot be polled.
    fn poll(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }
}

/// The pool job of a role step: id `step-<task>-a1`, bound to the step task,
/// budget resolved from the execution kind (`step_budgets` override for a
/// thinking step, else the shared budget), never from the durable kind label
/// or the role seat. The tick adds the payload.
#[must_use]
pub fn role_job(
    ctx: &Ctx,
    run: &RunRow,
    step: &StepRow,
    kind: &str,
    execution: RoleKind,
    role: &str,
    brief: &str,
) -> JobSpec {
    let mut spec = JobSpec::new(
        format!("step-{}-a1", step.task),
        kind,
        role,
        format!("{} ({} r{})", step.node, run.need, run.run),
        brief,
    );
    spec.task = Some(step.task.clone());
    spec.budget = ctx.cfg.dispatch.step_budget(execution);
    spec
}
