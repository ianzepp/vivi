//! `LanderWorkflow`: one long-lived workflow that receives land requests
//! by signal, batches them, gates the candidate, merges a passing batch,
//! and bisects a failing one until exactly the bad units are ejected. Each
//! slice gets its result back by signal.

use std::time::Duration;

use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ActivityOptions, SignalWorkflowOptions, SyncWorkflowContext, WorkflowContext, WorkflowResult,
    WorkflowTermination,
};

use crate::activities::Stand;
use crate::slice::SliceWorkflow;
use crate::types::{GateResult, LandRequest, LandResult, LanderInput};

#[workflow]
#[derive(Default)]
pub struct LanderWorkflow {
    queue: Vec<LandRequest>,
}

#[workflow_methods]
impl LanderWorkflow {
    #[run]
    pub async fn run(ctx: &mut WorkflowContext<Self>, input: LanderInput) -> WorkflowResult<()> {
        loop {
            ctx.wait_condition(|s| !s.queue.is_empty()).await?;
            ctx.timer(Duration::from_millis(input.batch_window_ms))
                .await;
            let batch = ctx.state_mut(|s| {
                let n = s.queue.len().min(input.max_batch.max(1));
                s.queue.drain(..n).collect::<Vec<_>>()
            });
            let results = land_batch(ctx, &input, batch).await?;
            for (request, result) in results {
                notify(ctx, request, result).await;
            }
        }
    }

    #[signal]
    pub fn submit(&mut self, _ctx: &mut SyncWorkflowContext<Self>, request: LandRequest) {
        self.queue.push(request);
    }
}

/// Gate the batch; on red, split it in half and gate each half, until a
/// red unit stands alone. Halves are gated in order, so a later half is
/// gated on top of the earlier half once that one has merged.
async fn land_batch(
    ctx: &WorkflowContext<LanderWorkflow>,
    input: &LanderInput,
    batch: Vec<LandRequest>,
) -> Result<Vec<(LandRequest, LandResult)>, WorkflowTermination> {
    let opts = lander_options(input);
    let mut results = Vec::new();
    let mut stack = vec![batch];
    while let Some(mut candidate) = stack.pop() {
        let gate = ctx
            .execute_activity(Stand::gate, candidate.clone(), opts.clone())
            .await?;
        match gate {
            GateResult::Pass => {
                ctx.execute_activity(Stand::merge, candidate.clone(), opts.clone())
                    .await?;
                results.extend(candidate.into_iter().map(|r| (r, LandResult::Merged)));
            }
            GateResult::Fail { text } if candidate.len() == 1 => {
                results.extend(candidate.into_iter().map(|r| {
                    (
                        r,
                        LandResult::Ejected {
                            cause: text.clone(),
                        },
                    )
                }));
            }
            GateResult::Fail { .. } => {
                let right = candidate.split_off(candidate.len() / 2);
                stack.push(right);
                stack.push(candidate);
            }
        }
    }
    Ok(results)
}

/// Gate and merge must not drop units, so they retry without limit.
fn lander_options(input: &LanderInput) -> ActivityOptions {
    ActivityOptions::with_start_to_close_timeout(Duration::from_millis(input.step_cap_ms))
        .retry_policy(
            temporalio_common::RetryPolicy::builder()
                .initial_interval(Duration::from_millis(100))
                .build(),
        )
        .build()
}

/// Tell a slice how its request ended. A slice that no longer runs has no
/// one left to tell, so a failed signal is dropped.
async fn notify(ctx: &WorkflowContext<LanderWorkflow>, request: LandRequest, result: LandResult) {
    let slice = ctx.external_workflow(request.reply_to, None);
    let _ = slice
        .signal(
            SliceWorkflow::land_result,
            result,
            SignalWorkflowOptions::default(),
        )
        .await;
}
