//! Release milestone handler: the `wait` node answers `ready` once every
//! prerequisite need of the run's need has landed on main.

use self::prereq::{Prereq, PrereqState, prerequisites};
use crate::error::VivariumError;
use crate::letter::work::StepFront;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, NeedStatus, TaskState};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::jobs::JobStore;

mod prereq;

/// The word the milestone answers when every prerequisite has landed.
const READY: &str = "ready";

/// Alarm kind: a prerequisite was closed without landing; the step is held.
pub(crate) const MILESTONE_UNLANDED: &str = "flow-release-prereq-unlanded";

/// Alarm kind: a prerequisite is open but held by an operator-only state.
pub(crate) const MILESTONE_PREREQ_HELD: &str = "flow-release-prereq-held";

/// Waits for the need's prerequisite needs; `poll` answers when all landed.
struct Milestone;

impl StepHandler for Milestone {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }

    fn poll(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<Option<String>, VivariumError> {
        let need = &cx.run.need;
        let prereqs = prerequisites(cx.store, &Reader(cx.mail), need).map_err(|error| {
            VivariumError::Message(format!(
                "release {need}: expected its prerequisites from the backlog graph; actual: {error}"
            ))
        })?;
        let mut unlanded = false;
        for Prereq { handle, state } in &prereqs {
            match state {
                PrereqState::ClosedUnlanded(outcome) => {
                    unlanded = true;
                    let text = format!(
                        "release {need} expects {handle} landed on main; actual: closed, outcome `{outcome}`"
                    );
                    raise_once(
                        cx.ctx,
                        MILESTONE_UNLANDED,
                        &format!("{need}:{handle}"),
                        &text,
                    );
                }
                PrereqState::Held(held) => {
                    let text = format!(
                        "release {need} waits for {handle}; expected it to land; actual: open and held at {held}"
                    );
                    raise_once(
                        cx.ctx,
                        MILESTONE_PREREQ_HELD,
                        &format!("{need}:{handle}"),
                        &text,
                    );
                }
                PrereqState::Landed | PrereqState::Open => {}
            }
        }
        if unlanded {
            cx.store
                .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))?;
            return Ok(None);
        }
        let all_landed = prereqs.iter().all(|p| p.state == PrereqState::Landed);
        Ok(all_landed.then(|| READY.to_owned()))
    }
}

/// A sized [`MailReader`] over the step context's mail, which is a trait
/// object; `prerequisites` takes a sized reader.
struct Reader<'a>(&'a dyn SliceMail);

impl MailReader for Reader<'_> {
    fn task_state(&self, handle: &str) -> Result<TaskState, VivariumError> {
        self.0.task_state(handle)
    }

    fn need_state(&self, need: &str) -> Result<NeedStatus, VivariumError> {
        self.0.need_state(need)
    }

    fn need_marks(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.0.need_marks(need)
    }

    fn open_record(&self, need: &str) -> Result<Option<(String, String)>, VivariumError> {
        self.0.open_record(need)
    }

    fn bound_units(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.0.bound_units(need)
    }

    fn sentinel_count(&self, need: &str) -> Result<usize, VivariumError> {
        self.0.sentinel_count(need)
    }

    fn need_prerequisites(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.0.need_prerequisites(need)
    }

    fn last_closed_record(&self, need: &str) -> Result<Option<String>, VivariumError> {
        self.0.last_closed_record(need)
    }

    fn sent_step(&self, subject: &str, step: &StepFront) -> Result<Option<String>, VivariumError> {
        self.0.sent_step(subject, step)
    }
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Release, NodeClass::Milestone, Box::new(Milestone));
}

#[cfg(test)]
#[path = "milestone_test.rs"]
mod tests;
