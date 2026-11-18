//! `SliceWorkflow`: implement → check → review → land, on one branch.
//!
//! A red check, a review change request or a Lander ejection sends the
//! slice back to implement with the failure text, on the same branch, from
//! where the last round left it. After `max_work_rounds` the slice parks.

use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    SignalWorkflowOptions, SyncWorkflowContext, WorkflowContext, WorkflowResult,
    WorkflowTermination,
};

use crate::activities::Stand;
use crate::lander::LanderWorkflow;
use crate::slice_workflow_id;
use crate::steps::{agent_options, run_step, step_options};
use crate::types::{
    CheckResult, Checkpoint, ImplementInput, LandRequest, LandResult, ParkCause, ReviewResult,
    SliceInput, SliceOutcome, StepInput,
};

#[workflow]
#[derive(Default)]
pub struct SliceWorkflow {
    land_result: Option<LandResult>,
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
        let slice = input.slice.id.clone();
        let mut feedback = None;
        let mut resume = None;
        let mut last_failure = String::new();
        let rounds = input.tuning.max_work_rounds;
        for round in 1..=rounds {
            let ended = run_round(ctx, &input, round, feedback.take(), resume.take()).await?;
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

    /// The Lander's answer to this slice's land request.
    #[signal]
    pub fn land_result(&mut self, _ctx: &mut SyncWorkflowContext<Self>, result: LandResult) {
        self.land_result = Some(result);
    }
}

/// The branch a slice works on, the same for every round.
pub fn slice_branch(need: &str, slice: &str) -> String {
    format!("vivi/{need}/{slice}")
}

async fn run_round(
    ctx: &WorkflowContext<SliceWorkflow>,
    input: &SliceInput,
    round: u32,
    feedback: Option<String>,
    resume: Option<Checkpoint>,
) -> Result<Round, WorkflowTermination> {
    let tuning = &input.tuning;
    let implement = ImplementInput {
        need: input.need.clone(),
        slice: input.slice.id.clone(),
        branch: slice_branch(&input.need, &input.slice.id),
        round,
        feedback,
        resume,
    };
    let opts = agent_options(tuning);
    let checkpoint = match run_step(
        ctx,
        "implement",
        || Stand::implement,
        implement,
        opts,
        tuning,
    )
    .await
    {
        Ok(checkpoint) => checkpoint,
        Err(cause) => return Ok(Round::Park(cause)),
    };
    let step = StepInput {
        need: input.need.clone(),
        slice: input.slice.id.clone(),
        round,
        head: checkpoint.head.clone(),
    };
    if let Some(ended) = check_and_review(ctx, input, step, &checkpoint).await {
        return Ok(ended);
    }
    land(ctx, input, checkpoint).await
}

/// `Some` when the round ends before landing.
async fn check_and_review(
    ctx: &WorkflowContext<SliceWorkflow>,
    input: &SliceInput,
    step: StepInput,
    checkpoint: &Checkpoint,
) -> Option<Round> {
    let tuning = &input.tuning;
    let again = |text: String| Round::Again {
        text,
        checkpoint: checkpoint.clone(),
    };
    let opts = step_options(tuning);
    match run_step(
        ctx,
        "check",
        || Stand::check,
        step.clone(),
        opts.clone(),
        tuning,
    )
    .await
    {
        Err(cause) => return Some(Round::Park(cause)),
        Ok(CheckResult::Red { text }) => return Some(again(text)),
        Ok(CheckResult::Green) => {}
    }
    match run_step(ctx, "review", || Stand::review, step, opts, tuning).await {
        Err(cause) => Some(Round::Park(cause)),
        Ok(ReviewResult::Changes { text }) => Some(again(text)),
        Ok(ReviewResult::Approve) => None,
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
