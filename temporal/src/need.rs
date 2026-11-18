//! `NeedWorkflow`: plan → one child `SliceWorkflow` per slice → close.
//!
//! The workflow id is `need-<handle>`, started with a reject-duplicate id
//! policy, so a second start of the same need is refused by the server.
//! A slice that runs out of rounds or attempts parks the need in the record
//! with a typed cause. Nothing restarts at plan.

use std::time::Duration;

use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::workflows::join_all;
use temporalio_sdk::{
    ActivityOptions, ChildWorkflowOptions, WorkflowContext, WorkflowIdReusePolicy, WorkflowResult,
    WorkflowTermination,
};

use crate::activities::Stand;
use crate::slice::SliceWorkflow;
use crate::slice_workflow_id;
use crate::steps::{run_step, step_options};
use crate::types::{
    NeedInput, NeedOutcome, ParkCause, PlanInput, RecordCall, SliceInput, SliceOutcome, SliceSpec,
};

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
        let plan = PlanInput { need: need.clone() };
        let opts = step_options(&input.tuning);
        let slices = match run_step(ctx, "plan", || Stand::plan, plan, opts, &input.tuning).await {
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
        for outcome in outcomes {
            match outcome {
                SliceOutcome::Landed { slice, .. } => landed.push(slice),
                SliceOutcome::Parked { slice, cause } => {
                    return park(ctx, &input, slice, cause).await;
                }
            }
        }
        let summary = format!("landed {}", landed.join(", "));
        record(ctx, &input, RecordCall::Close { need, summary }).await?;
        Ok(NeedOutcome::Closed { landed })
    }
}

async fn run_slice(
    ctx: WorkflowContext<NeedWorkflow>,
    input: NeedInput,
    spec: SliceSpec,
) -> SliceOutcome {
    let opts = ChildWorkflowOptions::builder()
        .workflow_id(slice_workflow_id(&input.handle, &spec.id))
        .id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)
        .build();
    let slice = spec.id.clone();
    let child = SliceInput {
        need: input.handle,
        slice: spec,
        lander_id: input.lander_id,
        tuning: input.tuning,
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
        Err(err) => failed(err.to_string()),
    }
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
    ctx.execute_activity(Stand::record, call, opts).await?;
    Ok(())
}
