//! `SliceWorkflow`: implement → check → review → land, on one branch.
//!
//! A red check, a review change request or a Lander ejection sends the
//! slice back to implement with the failure text, on the same branch, from
//! where the last round left it. After `max_work_rounds` the slice parks.
//!
//! Before its first round the slice asks the Lander for admission
//! (backpressure and the hot-file mutex, `admission.rs`) and waits for it;
//! when it ends, landed or parked, it releases its lease.

use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    SignalWorkflowOptions, SyncWorkflowContext, WorkflowContext, WorkflowResult,
    WorkflowTermination,
};

use crate::defs::Act;
use crate::lander::LanderWorkflow;
use crate::routing::placements;
use crate::slice_workflow_id;
use crate::steps::{agent_options, run_placed, step_options};
use crate::types::{
    AdmitRequest, CheckResult, Checkpoint, ImplementInput, LandRequest, LandResult, ParkCause,
    ReviewResult, SliceInput, SliceOutcome, StepInput,
};

#[workflow]
#[derive(Default)]
pub struct SliceWorkflow {
    land_result: Option<LandResult>,
    admitted: bool,
}

/// How one work round ended.
enum Round {
    Landed {
        head: String,
    },
    Again {
        text: String,
        checkpoint: Checkpoint,
    },
    Park(ParkCause),
}

#[workflow_methods]
impl SliceWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: SliceInput,
    ) -> WorkflowResult<SliceOutcome> {
        if let Err(cause) = admit(ctx, &input).await? {
            let slice = input.slice.id.clone();
            return Ok(SliceOutcome::Parked { slice, cause });
        }
        let outcome = run_rounds(ctx, &input).await?;
        release(ctx, &input).await;
        Ok(outcome)
    }

    /// The Lander's answer to this slice's land request.
    #[signal]
    pub fn land_result(&mut self, _ctx: &mut SyncWorkflowContext<Self>, result: LandResult) {
        self.land_result = Some(result);
    }

    /// The Lander's leave to start.
    #[signal]
    pub fn admitted(&mut self, _ctx: &mut SyncWorkflowContext<Self>, _ok: bool) {
        self.admitted = true;
    }
}

/// Ask the Lander for leave to start and wait for it. The inner error is a
/// park cause: the Lander could not be reached.
async fn admit(
    ctx: &WorkflowContext<SliceWorkflow>,
    input: &SliceInput,
) -> Result<Result<(), ParkCause>, WorkflowTermination> {
    let request = AdmitRequest {
        reply_to: slice_workflow_id(&input.need, &input.slice.id),
        write_scope: input.slice.write_scope.clone(),
    };
    let lander = ctx.external_workflow(input.lander_id.clone(), None);
    let sent = lander
        .signal(
            LanderWorkflow::admit,
            request,
            SignalWorkflowOptions::default(),
        )
        .await;
    if let Err(err) = sent {
        return Ok(Err(ParkCause::InfraExhausted {
            step: "admit".to_string(),
            detail: err.to_string(),
        }));
    }
    ctx.wait_condition(|s| s.admitted).await?;
    Ok(Ok(()))
}

/// Give the lease back. A Lander that cannot be reached has no lease to
/// give back to, so a failed signal is dropped.
async fn release(ctx: &WorkflowContext<SliceWorkflow>, input: &SliceInput) {
    let key = slice_workflow_id(&input.need, &input.slice.id);
    let lander = ctx.external_workflow(input.lander_id.clone(), None);
    let _ = lander
        .signal(
            LanderWorkflow::release,
            key,
            SignalWorkflowOptions::default(),
        )
        .await;
}

async fn run_rounds(
    ctx: &WorkflowContext<SliceWorkflow>,
    input: &SliceInput,
) -> Result<SliceOutcome, WorkflowTermination> {
    let slice = input.slice.id.clone();
    let mut feedback = None;
    let mut resume = None;
    let mut last_failure = String::new();
    let rounds = input.tuning.max_work_rounds;
    for round in 1..=rounds {
        let ended = run_round(ctx, input, round, feedback.take(), resume.take()).await?;
        match ended {
            Round::Landed { head } => {
                return Ok(SliceOutcome::Landed {
                    slice,
                    head,
                    rounds: round,
                });
            }
            Round::Park(cause) => return Ok(SliceOutcome::Parked { slice, cause }),
            Round::Again { text, checkpoint } => {
                last_failure = text.clone();
                feedback = Some(text);
                resume = Some(checkpoint);
            }
        }
    }
    let cause = ParkCause::WorkExhausted {
        rounds,
        last_failure,
    };
    Ok(SliceOutcome::Parked { slice, cause })
}

/// The branch a slice works on, the same for every round, under the
/// default `vivi/` prefix.
pub fn slice_branch(need: &str, slice: &str) -> String {
    branch_for("vivi/", need, slice)
}

/// The branch a slice works on under a target's branch prefix.
pub fn branch_for(prefix: &str, need: &str, slice: &str) -> String {
    format!("{prefix}{need}/{slice}")
}

async fn run_round(
    ctx: &WorkflowContext<SliceWorkflow>,
    input: &SliceInput,
    round: u32,
    feedback: Option<String>,
    resume: Option<Checkpoint>,
) -> Result<Round, WorkflowTermination> {
    let tuning = &input.tuning;
    let routing = &input.routing;
    let key = slice_workflow_id(&input.need, &input.slice.id);
    let preferred = resume.as_ref().map(|c| c.host.clone());
    let places = placements(
        routing,
        &routing.agent,
        &routing.agent_hosts,
        preferred.as_deref(),
        &key,
    );
    let implement = ImplementInput {
        need: input.need.clone(),
        slice: input.slice.id.clone(),
        branch: branch_for(&input.branch_prefix, &input.need, &input.slice.id),
        round,
        feedback,
        resume,
        target: input.target.clone(),
        brief: input.slice.brief.clone(),
    };
    let opts = agent_options(tuning);
    let placed = run_placed(
        ctx,
        "implement",
        || Act::implement,
        implement,
        opts,
        tuning,
        &places,
    );
    let checkpoint = match placed.await {
        Ok((checkpoint, host)) => Checkpoint { host, ..checkpoint },
        Err(cause) => return Ok(Round::Park(cause)),
    };
    let step = StepInput {
        need: input.need.clone(),
        slice: input.slice.id.clone(),
        round,
        head: checkpoint.head.clone(),
        target: input.target.clone(),
        branch: checkpoint.branch.clone(),
        brief: input.slice.brief.clone(),
    };
    if let Some(ended) = check_and_review(ctx, input, step, &checkpoint).await {
        return Ok(ended);
    }
    land(ctx, input, checkpoint).await
}

/// `Some` when the round ends before landing. Check and review prefer the
/// host that holds the work in its warm tree.
async fn check_and_review(
    ctx: &WorkflowContext<SliceWorkflow>,
    input: &SliceInput,
    step: StepInput,
    checkpoint: &Checkpoint,
) -> Option<Round> {
    let tuning = &input.tuning;
    let routing = &input.routing;
    let key = slice_workflow_id(&input.need, &input.slice.id);
    let place = |template: &str| {
        placements(
            routing,
            template,
            &routing.agent_hosts,
            Some(&checkpoint.host),
            &key,
        )
    };
    let again = |text: String| Round::Again {
        text,
        checkpoint: checkpoint.clone(),
    };
    let opts = step_options(tuning);
    let check_places = place(&routing.check);
    let checked = run_placed(
        ctx,
        "check",
        || Act::check,
        step.clone(),
        opts.clone(),
        tuning,
        &check_places,
    );
    match checked.await {
        Err(cause) => return Some(Round::Park(cause)),
        Ok((CheckResult::Red { text }, _)) => return Some(again(text)),
        Ok((CheckResult::Green, _)) => {}
    }
    let review_places = place(&routing.review);
    let reviewed = run_placed(
        ctx,
        "review",
        || Act::review,
        step,
        opts,
        tuning,
        &review_places,
    );
    match reviewed.await {
        Err(cause) => Some(Round::Park(cause)),
        Ok((ReviewResult::Changes { text }, _)) => Some(again(text)),
        Ok((ReviewResult::Approve, _)) => None,
    }
}

async fn land(
    ctx: &WorkflowContext<SliceWorkflow>,
    input: &SliceInput,
    checkpoint: Checkpoint,
) -> Result<Round, WorkflowTermination> {
    let request = LandRequest {
        reply_to: slice_workflow_id(&input.need, &input.slice.id),
        need: input.need.clone(),
        slice: input.slice.id.clone(),
        head: checkpoint.head.clone(),
        branch: checkpoint.branch.clone(),
    };
    ctx.state_mut(|s| s.land_result = None);
    let lander = ctx.external_workflow(input.lander_id.clone(), None);
    let sent = lander
        .signal(
            LanderWorkflow::submit,
            request,
            SignalWorkflowOptions::default(),
        )
        .await;
    if let Err(err) = sent {
        let cause = ParkCause::InfraExhausted {
            step: "land".to_string(),
            detail: err.to_string(),
        };
        return Ok(Round::Park(cause));
    }
    ctx.wait_condition(|s| s.land_result.is_some()).await?;
    match ctx.state_mut(|s| s.land_result.take()) {
        Some(LandResult::Ejected { cause }) => Ok(Round::Again {
            text: cause,
            checkpoint,
        }),
        _ => Ok(Round::Landed {
            head: checkpoint.head,
        }),
    }
}
