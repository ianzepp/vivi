//! The real activities: thin wrappers over Vivi's pool, git, and the
//! `lab/lander` commands, with failures mapped onto the README's classes.
//! One `Real` registers on every queue a host serves; which activities
//! actually run there is decided by the queue each step is scheduled on.

pub mod agent;
pub mod check;
pub mod cmd;
pub mod git;
pub mod land;
pub mod pool;

use std::sync::Arc;

use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::config::Config;
use crate::defs;
use crate::record::NeedRecord;
use crate::types::{
    Candidate, CheckResult, Checkpoint, ComposeInput, ComposeResult, GateResult, ImplementInput,
    PlanInput, PublishResult, RecordCall, ReviewResult, SliceSpec, StepInput,
};

pub struct Real {
    /// The `--host` this worker runs as.
    pub host: String,
    pub config: Arc<Config>,
    pub record: Arc<dyn NeedRecord>,
}

#[activities]
impl Real {
    /// The slices the need was given, or one slice for the whole brief.
    /// An LLM planner is not built yet.
    #[activity(definition = defs::Plan)]
    pub async fn plan(
        self: Arc<Self>,
        _ctx: ActivityContext,
        input: PlanInput,
    ) -> Result<Vec<SliceSpec>, ActivityError> {
        Ok(plan_slices(&input))
    }

    #[activity(definition = defs::Implement)]
    pub async fn implement(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: ImplementInput,
    ) -> Result<Checkpoint, ActivityError> {
        let target = self.config.target(&input.target)?;
        agent::implement(target, &ctx, &input, &self.host).await
    }

    #[activity(definition = defs::Check)]
    pub async fn check(
        self: Arc<Self>,
        _ctx: ActivityContext,
        input: StepInput,
    ) -> Result<CheckResult, ActivityError> {
        check::check(self.config.target(&input.target)?, &input).await
    }

    #[activity(definition = defs::Review)]
    pub async fn review(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: StepInput,
    ) -> Result<ReviewResult, ActivityError> {
        agent::review(self.config.target(&input.target)?, &ctx, &input).await
    }

    #[activity(definition = defs::Compose)]
    pub async fn compose(
        self: Arc<Self>,
        _ctx: ActivityContext,
        input: ComposeInput,
    ) -> Result<ComposeResult, ActivityError> {
        let target = self.config.target(&input.target)?.clone();
        land::compose(&target, input).await
    }

    #[activity(definition = defs::Gate)]
    pub async fn gate(
        self: Arc<Self>,
        _ctx: ActivityContext,
        candidate: Candidate,
    ) -> Result<GateResult, ActivityError> {
        land::gate(self.config.target(&candidate.target)?, &candidate).await
    }

    #[activity(definition = defs::Publish)]
    pub async fn publish(
        self: Arc<Self>,
        _ctx: ActivityContext,
        candidate: Candidate,
    ) -> Result<PublishResult, ActivityError> {
        land::publish(self.config.target(&candidate.target)?, &candidate).await
    }

    #[activity(definition = defs::Record)]
    pub async fn record(
        self: Arc<Self>,
        _ctx: ActivityContext,
        call: RecordCall,
    ) -> Result<(), ActivityError> {
        let record = self.record.clone();
        tokio::task::spawn_blocking(move || record.apply(&call)).await??;
        Ok(())
    }
}

pub fn plan_slices(input: &PlanInput) -> Vec<SliceSpec> {
    if !input.slices.is_empty() {
        return input.slices.clone();
    }
    vec![SliceSpec {
        id: "s1".to_string(),
        write_scope: Vec::new(),
        brief: input.brief.clone(),
    }]
}
