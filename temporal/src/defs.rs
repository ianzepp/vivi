//! The activity interface the workflows call. Two implementations register
//! under these names: the stand-ins in `activities.rs` (tests) and the real
//! command wrappers in `real/` (workers). Workflows never name either one.

use temporalio_macros::activity_definitions;
use temporalio_sdk::activities::ActivityError;

use crate::types::{
    Candidate, CheckResult, Checkpoint, ComposeInput, ComposeResult, GateResult, ImplementInput,
    PlanInput, PublishResult, RecordCall, ReviewResult, SliceSpec, StepInput,
};

/// Failure type name for provider throttling (429, quota). Workflows wait
/// these out themselves, so they never spend an attempt.
pub const PROVIDER_FAILURE: &str = "provider";
/// Failure type name for a lost host, ssh, slot or git transport failure.
/// Temporal retries it, then the workflow moves to another host.
pub const INFRA_FAILURE: &str = "infra";
/// Failure type name for a reply the agent could not put in the required
/// form. Retried like infra, per the README's policy.
pub const FORMAT_FAILURE: &str = "format";

pub struct Act;

/// The marker types implementations name in `#[activity(definition = …)]`.
pub use act::{Check, Compose, Gate, Implement, Plan, Publish, Record, Review};

#[activity_definitions]
impl Act {
    #[activity(name = "plan")]
    pub fn plan(_input: PlanInput) -> Result<Vec<SliceSpec>, ActivityError> {
        unimplemented!()
    }

    #[activity(name = "implement")]
    pub fn implement(_input: ImplementInput) -> Result<Checkpoint, ActivityError> {
        unimplemented!()
    }

    #[activity(name = "check")]
    pub fn check(_input: StepInput) -> Result<CheckResult, ActivityError> {
        unimplemented!()
    }

    #[activity(name = "review")]
    pub fn review(_input: StepInput) -> Result<ReviewResult, ActivityError> {
        unimplemented!()
    }

    #[activity(name = "compose")]
    pub fn compose(_input: ComposeInput) -> Result<ComposeResult, ActivityError> {
        unimplemented!()
    }

    #[activity(name = "gate")]
    pub fn gate(_input: Candidate) -> Result<GateResult, ActivityError> {
        unimplemented!()
    }

    #[activity(name = "publish")]
    pub fn publish(_input: Candidate) -> Result<PublishResult, ActivityError> {
        unimplemented!()
    }

    #[activity(name = "record")]
    pub fn record(_input: RecordCall) -> Result<(), ActivityError> {
        unimplemented!()
    }
}
