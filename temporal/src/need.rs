//! `NeedWorkflow`: plan → one child `SliceWorkflow` per slice → close.
//!
//! The workflow id is `need-<handle>`, started with a reject-duplicate id
//! policy, so a second start of the same need is refused by the server.
//! A slice that runs out of rounds or attempts parks the need in the record
//! with a typed cause. Nothing restarts at plan.

use std::time::Duration;

use temporalio_common::error::IncomingError;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::workflows::join_all;
use temporalio_sdk::{
    ActivityOptions, ChildWorkflowOptions, WorkflowContext, WorkflowIdReusePolicy, WorkflowResult,
    WorkflowTermination,
};

use crate::defs::Act;
use crate::routing::placements;
use crate::slice::SliceWorkflow;
use crate::steps::{run_placed, run_step, step_options};
use crate::types::{
    NeedInput, NeedOutcome, ParkCause, PlanInput, RecordCall, SliceInput, SliceOutcome, SliceSpec,
};
use crate::{need_workflow_id, slice_workflow_id};

#[workflow]
#[derive(Default)]
pub struct NeedWorkflow;

#[workflow_methods]
impl NeedWorkflow {
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: NeedInput,
    ) -> WorkflowResult<NeedOutcome> {
        let need = input.handle.clone();
        record(
            ctx,
            &input,
            RecordCall::Status {
                need: need.clone(),
                status: "planning".into(),
            },
        )
        .await?;
        let plan = PlanInput {
            need: need.clone(),
            target: input.target.clone(),
            brief: input.brief.clone(),
            slices: input.slices.clone(),
        };
        let slices = match run_plan(ctx, &input, plan).await {
            Ok(slices) => slices,
            Err(cause) => return park(ctx, &input, "plan".into(), cause).await,
        };
        record(
            ctx,
            &input,
            RecordCall::Status {
                need: need.clone(),
                status: "building".into(),
            },
        )
        .await?;
        let children = slices
            .into_iter()
            .map(|s| run_slice(ctx.clone(), input.clone(), s));
        let outcomes = join_all(children).await;
        let mut landed = Vec::new();
        let mut parked = None;
        for outcome in outcomes {
            match outcome {
                SliceOutcome::Landed { slice, .. } => landed.push(slice),
                SliceOutcome::Stopped { slice } => return Ok(NeedOutcome::Stopped { slice }),
                SliceOutcome::Parked { slice, cause } => {
                    parked.get_or_insert((slice, cause));
                }
            }
        }
        if let Some((slice, cause)) = parked {
            return park(ctx, &input, slice, cause).await;
        }
        let summary = format!("landed {}", landed.join(", "));
        record(ctx, &input, RecordCall::Close { need, summary }).await?;
        Ok(NeedOutcome::Closed { landed })
    }
}

/// Plan on the control queue, or on `plan_hosts` when they are set.
async fn run_plan(
    ctx: &WorkflowContext<NeedWorkflow>,
    input: &NeedInput,
    plan: PlanInput,
) -> Result<Vec<SliceSpec>, ParkCause> {
    let tuning = &input.tuning;
    let routing = &input.routing;
    let opts = step_options(tuning);
    if routing.plan_hosts.is_empty() {
        return run_step(ctx, "plan", || Act::plan, plan, opts, tuning).await;
    }
    let key = need_workflow_id(&input.handle);
    let places = placements(routing, &routing.plan, &routing.plan_hosts, None, &key);
    let placed = run_placed(ctx, "plan", || Act::plan, plan, opts, tuning, &places).await;
    placed.map(|(slices, _)| slices)
}

async fn run_slice(
    ctx: WorkflowContext<NeedWorkflow>,
    input: NeedInput,
    spec: SliceSpec,
) -> SliceOutcome {
    // A need run starts each slice once, and two runs of one need never
    // overlap (the server refuses a running need's id), so the only repeat
    // start of a slice id is an asked-for rerun of its need. That rerun may
    // start the slice again; it resumes from the slice branch, and work
    // already on main is found merged by compose.
    let opts = ChildWorkflowOptions::builder()
        .workflow_id(slice_workflow_id(&input.handle, &spec.id))
        .id_reuse_policy(WorkflowIdReusePolicy::AllowDuplicate)
        .build();
    let slice = spec.id.clone();
    let child = SliceInput {
        need: input.handle,
        slice: spec,
        lander_id: input.lander_id,
        tuning: input.tuning,
        routing: input.routing,
        target: input.target,
        branch_prefix: input.branch_prefix,
    };
    let failed = |detail: String| SliceOutcome::Parked {
        slice: slice.clone(),
        cause: ParkCause::InfraExhausted {
            step: "slice".into(),
            detail,
        },
    };
    let started = match ctx
        .start_child_workflow(SliceWorkflow::run, child, opts)
        .await
    {
        Ok(started) => started,
        Err(err) => return failed(err.to_string()),
    };
    match started.result().await {
        Ok(outcome) => outcome,
        // An operator's terminate or cancel is not a failure of the work.
        Err(err) if stopped_by_operator(err.cause()) => SliceOutcome::Stopped { slice },
        Err(err) => failed(err.to_string()),
    }
}

fn stopped_by_operator(cause: Option<&IncomingError>) -> bool {
    matches!(
        cause,
        Some(IncomingError::Terminated(_) | IncomingError::Cancelled(_))
    )
}

async fn park(
    ctx: &WorkflowContext<NeedWorkflow>,
    input: &NeedInput,
    slice: String,
    cause: ParkCause,
) -> WorkflowResult<NeedOutcome> {
    let call = RecordCall::Park {
        need: input.handle.clone(),
        slice: slice.clone(),
        cause: cause.clone(),
    };
    record(ctx, input, call).await?;
    Ok(NeedOutcome::Parked { slice, cause })
}

/// Write to the Vivi record. The record is the system of record, so this
/// retries without limit rather than parking.
async fn record(
    ctx: &WorkflowContext<NeedWorkflow>,
    input: &NeedInput,
    call: RecordCall,
) -> Result<(), WorkflowTermination> {
    let opts = ActivityOptions::with_start_to_close_timeout(Duration::from_millis(
        input.tuning.step_cap_ms,
    ))
    .retry_policy(
        temporalio_common::RetryPolicy::builder()
            .initial_interval(Duration::from_millis(input.tuning.infra_retry_ms))
            .build(),
    )
    .build();
    ctx.execute_activity(Act::record, call, opts).await?;
    Ok(())
}
