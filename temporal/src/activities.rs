//! Stand-in activities. Each one consults the world's fault script, then
//! behaves like the real step would: an LLM step can be throttled, an agent
//! step pushes commits and heartbeats, a machine step can come back red.

use std::sync::Arc;
use std::time::Duration;

use temporalio_macros::activities;
use temporalio_sdk::ApplicationFailure;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::error::SpikeError;
use crate::fault::{FaultKind, fault_at};
use crate::record::NeedRecord;
use crate::types::{
    CheckResult, Checkpoint, GateResult, ImplementInput, LandRequest, PlanInput, RecordCall,
    ReviewResult, SliceSpec, StepInput,
};
use crate::world::{Event, ImplementStart, World};

/// Failure type name for provider throttling (429, quota). Workflows wait
/// these out themselves, so they never spend an attempt.
pub const PROVIDER_FAILURE: &str = "provider";

pub struct Stand {
    pub world: Arc<World>,
    pub record: Arc<dyn NeedRecord>,
}

#[activities]
impl Stand {
    #[activity]
    pub async fn plan(
        self: Arc<Self>,
        _ctx: ActivityContext,
        input: PlanInput,
    ) -> Result<Vec<SliceSpec>, ActivityError> {
        let world = &self.world;
        let call = world.next_call("plan", &input.need)?;
        let script = world.need(&input.need)?;
        if fault_at(&script.plan, call) == Some(FaultKind::Quota) {
            return Err(throttled(world, "plan", &input.need)?);
        }
        world.log(Event::Planned {
            need: input.need.clone(),
        })?;
        let slices = script.slices.iter().map(|s| SliceSpec {
            id: s.id.clone(),
            write_scope: s.write_scope.clone(),
        });
        Ok(slices.collect())
    }

    #[activity]
    pub async fn implement(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: ImplementInput,
    ) -> Result<Checkpoint, ActivityError> {
        let world = &self.world;
        let call = world.next_call("implement", &input.slice)?;
        let fault = fault_at(&world.slice(&input.need, &input.slice)?.implement, call);
        if fault == Some(FaultKind::Quota) {
            return Err(throttled(world, "implement", &input.slice)?);
        }
        let resume = resume_point(&ctx, &input)?;
        world.log(Event::ImplementStarted(ImplementStart {
            slice: input.slice.clone(),
            round: input.round,
            attempt: ctx.info().attempt,
            branch: input.branch.clone(),
            resumed_from: resume.as_ref().map(|c| c.head.clone()),
            feedback: input.feedback.clone(),
            at: world.elapsed(),
        }))?;
        run_steps(world, &ctx, &input, resume, fault).await
    }

    #[activity]
    pub async fn check(
        self: Arc<Self>,
        _ctx: ActivityContext,
        input: StepInput,
    ) -> Result<CheckResult, ActivityError> {
        let world = &self.world;
        let call = world.next_call("check", &input.slice)?;
        let red =
            fault_at(&world.slice(&input.need, &input.slice)?.check, call) == Some(FaultKind::Red);
        {
            let _running = world.enter(&input.slice)?;
            tokio::time::sleep(world.timing.step).await;
        }
        world.log(Event::Checked {
            slice: input.slice.clone(),
            round: input.round,
            green: !red,
        })?;
        if red {
            let text = format!(
                "check red at {}: {}::acceptance failed",
                input.head, input.slice
            );
            return Ok(CheckResult::Red { text });
        }
        Ok(CheckResult::Green)
    }

    #[activity]
    pub async fn review(
        self: Arc<Self>,
        _ctx: ActivityContext,
        input: StepInput,
    ) -> Result<ReviewResult, ActivityError> {
        let world = &self.world;
        let call = world.next_call("review", &input.slice)?;
        let fault = fault_at(&world.slice(&input.need, &input.slice)?.review, call);
        if fault == Some(FaultKind::Quota) {
            return Err(throttled(world, "review", &input.slice)?);
        }
        {
            let _running = world.enter(&input.slice)?;
            tokio::time::sleep(world.timing.step).await;
        }
        let changes = fault == Some(FaultKind::Changes);
        world.log(Event::Reviewed {
            slice: input.slice.clone(),
            round: input.round,
            approved: !changes,
        })?;
        if changes {
            let text = format!(
                "review of {}: rename the helper and cover the error path",
                input.head
            );
            return Ok(ReviewResult::Changes { text });
        }
        Ok(ReviewResult::Approve)
    }

    /// The fast gate on a candidate: main plus the batch.
    #[activity]
    pub async fn gate(
        self: Arc<Self>,
        _ctx: ActivityContext,
        batch: Vec<LandRequest>,
    ) -> Result<GateResult, ActivityError> {
        let world = &self.world;
        tokio::time::sleep(world.timing.step).await;
        let mut broken = Vec::new();
        for req in &batch {
            if world.slice(&req.need, &req.slice)?.bad {
                broken.push(req.slice.clone());
            }
        }
        let slices: Vec<String> = batch.iter().map(|r| r.slice.clone()).collect();
        world.log(Event::Gate {
            batch: slices,
            pass: broken.is_empty(),
        })?;
        if broken.is_empty() {
            return Ok(GateResult::Pass);
        }
        Ok(GateResult::Fail {
            text: format!(
                "gate red: workspace tests fail with {} in the batch",
                broken.join(", ")
            ),
        })
    }

    #[activity]
    pub async fn merge(
        self: Arc<Self>,
        _ctx: ActivityContext,
        batch: Vec<LandRequest>,
    ) -> Result<(), ActivityError> {
        let slices: Vec<String> = batch.into_iter().map(|r| r.slice).collect();
        Ok(self.world.merge(&slices)?)
    }

    #[activity]
    pub async fn record(
        self: Arc<Self>,
        _ctx: ActivityContext,
        call: RecordCall,
    ) -> Result<(), ActivityError> {
        Ok(self.record.apply(&call)?)
    }
}

/// A provider 429: typed, carrying the Retry-After, and not retried by
/// Temporal. The workflow waits it out without counting an attempt.
fn throttled(world: &World, step: &str, slice: &str) -> Result<ActivityError, SpikeError> {
    world.log(Event::Throttled {
        step: step.to_string(),
        slice: slice.to_string(),
        at: world.elapsed(),
    })?;
    let failure = ApplicationFailure::builder(format!("{step}: provider returned 429"))
        .type_name(PROVIDER_FAILURE.to_string())
        .non_retryable(true)
        .next_retry_delay(world.timing.retry_after)
        .build();
    Ok(ActivityError::application(failure))
}

/// Where this attempt starts: the last heartbeat of an earlier attempt of
/// this same activity (a Temporal retry), else where the last round left
/// the branch.
fn resume_point(
    ctx: &ActivityContext,
    input: &ImplementInput,
) -> Result<Option<Checkpoint>, ActivityError> {
    let beat: Option<Checkpoint> = ctx.heartbeat_details().deserialize()?;
    if beat.is_some() {
        return Ok(beat);
    }
    Ok(input
        .resume
        .clone()
        .map(|c| Checkpoint { steps_done: 0, ..c }))
}

async fn run_steps(
    world: &World,
    ctx: &ActivityContext,
    input: &ImplementInput,
    resume: Option<Checkpoint>,
    fault: Option<FaultKind>,
) -> Result<Checkpoint, ActivityError> {
    let mut head = resume.as_ref().map(|c| c.head.clone());
    let mut done = resume.as_ref().map_or(0, |c| c.steps_done);
    let mut pushed_this_attempt = 0;
    while done < world.timing.steps_per_round {
        done += 1;
        let commit = format!("{}-r{}-s{}", input.slice, input.round, done);
        {
            let _running = world.enter(&input.slice)?;
            tokio::time::sleep(world.timing.step).await;
            world.push(&input.branch, head.as_deref(), &commit)?;
        }
        world.log(Event::Pushed {
            slice: input.slice.clone(),
            branch: input.branch.clone(),
            commit: commit.clone(),
        })?;
        head = Some(commit.clone());
        let checkpoint = Checkpoint {
            branch: input.branch.clone(),
            head: commit,
            steps_done: done,
        };
        ctx.record_heartbeat(checkpoint.clone()).await?;
        pushed_this_attempt += 1;
        if pushed_this_attempt == 1
            && let Some(kind @ (FaultKind::Lost | FaultKind::Timeout)) = fault
        {
            return Err(go_dark(world, ctx, &input.slice, kind, checkpoint).await?);
        }
    }
    Ok(Checkpoint {
        branch: input.branch.clone(),
        head: head.unwrap_or_default(),
        steps_done: done,
    })
}

/// Simulate a host that stops making progress after one pushed commit.
/// `Lost` stops heartbeating, so the heartbeat timeout fires. `Timeout`
/// keeps heartbeating, so the start-to-close cap fires. Either way the
/// result of this attempt is discarded by the server.
async fn go_dark(
    world: &World,
    ctx: &ActivityContext,
    slice: &str,
    kind: FaultKind,
    checkpoint: Checkpoint,
) -> Result<ActivityError, SpikeError> {
    world.log(Event::WentDark {
        slice: slice.to_string(),
        kind,
    })?;
    let beat = ctx
        .info()
        .heartbeat_timeout
        .map_or(Duration::from_millis(100), |t| t / 4);
    let deadline = tokio::time::Instant::now() + world.timing.dark_hold;
    while tokio::time::Instant::now() < deadline && !ctx.is_cancelled() {
        if kind == FaultKind::Timeout && ctx.record_heartbeat(checkpoint.clone()).await.is_err() {
            break;
        }
        tokio::time::sleep(beat).await;
    }
    Ok(ActivityError::cancelled())
}
