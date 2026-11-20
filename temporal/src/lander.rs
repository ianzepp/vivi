//! `LanderWorkflow`: one long-lived workflow per target. It runs two loops
//! side by side:
//!
//! - **Admission.** Slices ask (`admit`) before their first round and give
//!   their lease back (`release`) when they end. The Lander grants leave
//!   under backpressure and the hot-file mutex (`admission.rs`).
//! - **Landing.** Slices submit land requests (`submit`). The Lander waits
//!   one batch window, composes a candidate on the lane (units that
//!   conflict are ejected at once), gates it on a gate host, and publishes
//!   it with a fenced push. A red gate is bisected until each bad unit
//!   stands alone and is ejected. Each slice gets its result by signal.

use std::time::Duration;

use futures::FutureExt;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ActivityOptions, ContinueAsNewOptions, SignalWorkflowOptions, SyncWorkflowContext,
    WorkflowContext, WorkflowContextView, WorkflowResult, WorkflowTermination,
};

use crate::admission::Admission;
use crate::defs::Act;
use crate::routing::placements;
use crate::slice::SliceWorkflow;
use crate::types::{
    AdmitRequest, Candidate, ComposeInput, ComposeResult, DEFAULT_ROLLOVER_EVENTS, GateResult,
    LandRequest, LandResult, LanderCarry, LanderInput, PublishResult,
};

/// Publishes refused because main moved, before a batch gives up.
const MAX_STALE: u32 = 3;

#[workflow]
pub struct LanderWorkflow {
    input: LanderInput,
    queue: Vec<LandRequest>,
    in_flight: usize,
    admission: Admission,
    /// Set when the history is long enough to continue as new: no new
    /// batch starts, and the admission loop continues as new once no batch
    /// is in flight.
    rollover: bool,
}

impl LanderWorkflow {
    /// Units queued or being landed: the backlog backpressure measures.
    fn backlog(&self) -> usize {
        self.queue.len() + self.in_flight
    }
}

#[workflow_methods]
impl LanderWorkflow {
    /// Build the state from the input, carried state included, before any
    /// signal is applied. Restoring it in `run` instead would overwrite the
    /// signals a continued run receives before its first task.
    #[init]
    pub fn new(_ctx: &WorkflowContextView, input: LanderInput) -> Self {
        let carry = input.carry.clone().unwrap_or_default();
        LanderWorkflow {
            input,
            queue: carry.queue,
            in_flight: 0,
            admission: carry.admission,
            rollover: false,
        }
    }

    #[run]
    pub async fn run(ctx: &mut WorkflowContext<Self>) -> WorkflowResult<()> {
        let input = ctx.state(|s| s.input.clone());
        let admit = admission_loop(ctx.clone(), input.clone()).fuse();
        let land = landing_loop(ctx.clone(), input).fuse();
        futures::pin_mut!(admit, land);
        // Both loops run forever; the first to end (continue-as-new, or an
        // error) ends the run.
        temporalio_sdk::workflows::select! {
            ended = admit => ended?,
            ended = land => ended?,
        }
        Ok(())
    }

    #[signal]
    pub fn submit(&mut self, _ctx: &mut SyncWorkflowContext<Self>, request: LandRequest) {
        self.queue.push(request);
    }

    #[signal]
    pub fn admit(&mut self, _ctx: &mut SyncWorkflowContext<Self>, request: AdmitRequest) {
        self.admission.request(request);
    }

    #[signal]
    pub fn release(&mut self, _ctx: &mut SyncWorkflowContext<Self>, key: String) {
        self.admission.release(&key);
    }
}

async fn admission_loop(
    ctx: WorkflowContext<LanderWorkflow>,
    input: LanderInput,
) -> Result<(), WorkflowTermination> {
    loop {
        let policy = input.admission.clone();
        ctx.wait_condition(move |s| {
            s.admission.has_grantable(&policy, s.backlog()) || (s.rollover && s.in_flight == 0)
        })
        .await?;
        if ctx.state(|s| s.rollover && s.in_flight == 0) {
            return continue_as_new(&ctx, &input);
        }
        let granted = ctx.state_mut(|s| {
            let backlog = s.backlog();
            s.admission.grant(&input.admission, backlog)
        });
        for request in granted {
            let slice = ctx.external_workflow(request.reply_to.clone(), None);
            let sent = slice
                .signal(
                    SliceWorkflow::admitted,
                    true,
                    SignalWorkflowOptions::default(),
                )
                .await;
            if sent.is_err() {
                ctx.state_mut(|s| s.admission.release(&request.reply_to));
            }
        }
        check_rollover(&ctx, &input);
    }
}

/// Mark the run for continue-as-new when its history is long enough.
fn check_rollover(ctx: &WorkflowContext<LanderWorkflow>, input: &LanderInput) {
    let limit = match input.rollover_events {
        0 => DEFAULT_ROLLOVER_EVENTS,
        n => n,
    };
    if ctx.continue_as_new_suggested() || ctx.history_length() >= limit {
        ctx.state_mut(|s| s.rollover = true);
    }
}

/// Continue as new, handing the queue and the admission book over.
fn continue_as_new(
    ctx: &WorkflowContext<LanderWorkflow>,
    input: &LanderInput,
) -> Result<(), WorkflowTermination> {
    let carry = ctx.state(|s| LanderCarry {
        queue: s.queue.clone(),
        admission: s.admission.clone(),
    });
    let next = LanderInput {
        carry: Some(carry),
        ..input.clone()
    };
    match ctx.continue_as_new(next, ContinueAsNewOptions::default()) {
        Err(termination) => Err(termination),
        Ok(never) => match never {},
    }
}

async fn landing_loop(
    ctx: WorkflowContext<LanderWorkflow>,
    input: LanderInput,
) -> Result<(), WorkflowTermination> {
    loop {
        ctx.wait_condition(|s| !s.queue.is_empty() && !s.rollover)
            .await?;
        ctx.timer(Duration::from_millis(input.batch_window_ms))
            .await;
        let batch = ctx.state_mut(|s| {
            let n = s.queue.len().min(input.max_batch.max(1));
            let batch: Vec<LandRequest> = s.queue.drain(..n).collect();
            s.in_flight = batch.len();
            batch
        });
        let results = land_batch(&ctx, &input, batch).await?;
        // The batch stays in flight until every slice has its result, so a
        // continue-as-new never cuts a notification short.
        for (request, result) in results {
            notify(&ctx, request, result).await;
        }
        ctx.state_mut(|s| s.in_flight = 0);
        check_rollover(&ctx, &input);
    }
}

/// Compose, gate and publish the batch; on red, split it in half and do
/// the same for each half, until a red unit stands alone. Halves are
/// composed in order, so a later half lands on top of an earlier one.
async fn land_batch(
    ctx: &WorkflowContext<LanderWorkflow>,
    input: &LanderInput,
    batch: Vec<LandRequest>,
) -> Result<Vec<(LandRequest, LandResult)>, WorkflowTermination> {
    let mut results = Vec::new();
    let mut stack = vec![batch];
    let mut stale = 0;
    while let Some(group) = stack.pop() {
        let composed = compose(ctx, input, group).await?;
        results.extend(
            composed
                .already
                .into_iter()
                .map(|r| (r, LandResult::Merged)),
        );
        for ex in composed.excluded {
            let cause = format!("compose: {}", ex.reason);
            results.push((ex.unit, LandResult::Ejected { cause }));
        }
        let Some(mut candidate) = composed.candidate else {
            continue;
        };
        match gate(ctx, input, &candidate).await? {
            GateResult::Pass => match publish(ctx, input, &candidate).await? {
                PublishResult::Published { .. } => {
                    let merged = candidate.units.into_iter();
                    results.extend(merged.map(|r| (r, LandResult::Merged)));
                }
                PublishResult::Stale { text } if stale >= MAX_STALE => {
                    results.extend(eject_all(candidate.units, &text));
                }
                PublishResult::Stale { .. } => {
                    stale += 1;
                    stack.push(candidate.units);
                }
            },
            GateResult::Fail { text } if candidate.units.len() == 1 => {
                results.extend(eject_all(candidate.units, &text));
            }
            GateResult::Fail { .. } => {
                let right = candidate.units.split_off(candidate.units.len() / 2);
                stack.push(right);
                stack.push(candidate.units);
            }
        }
    }
    Ok(results)
}

fn eject_all(units: Vec<LandRequest>, text: &str) -> Vec<(LandRequest, LandResult)> {
    units
        .into_iter()
        .map(|r| {
            let cause = text.to_string();
            (r, LandResult::Ejected { cause })
        })
        .collect()
}

async fn compose(
    ctx: &WorkflowContext<LanderWorkflow>,
    input: &LanderInput,
    units: Vec<LandRequest>,
) -> Result<ComposeResult, WorkflowTermination> {
    let opts = lander_options(input.step_cap_ms, Some(input.routing.lane.clone()));
    let request = ComposeInput {
        target: input.target.clone(),
        units,
    };
    Ok(ctx.execute_activity(Act::compose, request, opts).await?)
}

async fn publish(
    ctx: &WorkflowContext<LanderWorkflow>,
    input: &LanderInput,
    candidate: &Candidate,
) -> Result<PublishResult, WorkflowTermination> {
    let opts = lander_options(input.step_cap_ms, Some(input.routing.lane.clone()));
    Ok(ctx
        .execute_activity(Act::publish, candidate.clone(), opts)
        .await?)
}

/// Gate on the verify hosts in turn, the target's home first: its
/// verification tree has already built the checked units. A host that does
/// not take the task in time, or fails it three times, passes it on; after
/// every host the Lander waits and starts over, because a gate must not
/// drop units.
async fn gate(
    ctx: &WorkflowContext<LanderWorkflow>,
    input: &LanderInput,
    candidate: &Candidate,
) -> Result<GateResult, WorkflowTermination> {
    let routing = &input.routing;
    let places = placements(
        routing,
        &routing.verify,
        &routing.verify_hosts,
        routing.home(&input.target),
        &candidate.id,
    );
    loop {
        for place in &places {
            let mut opts = lander_options(input.gate_cap_ms, Some(place.queue.clone()));
            opts.schedule_to_start_timeout = Some(place.wait);
            opts.retry_policy = Some(
                temporalio_common::RetryPolicy::builder()
                    .initial_interval(Duration::from_millis(100))
                    .maximum_attempts(3)
                    .build(),
            );
            let ran = ctx
                .execute_activity(Act::gate, candidate.clone(), opts)
                .await;
            if let Ok(result) = ran {
                return Ok(result);
            }
        }
        ctx.timer(Duration::from_millis(routing.affinity_wait_ms))
            .await;
    }
}

/// Compose and publish must not drop units, so they retry without limit.
fn lander_options(cap_ms: u64, queue: Option<String>) -> ActivityOptions {
    let mut opts = ActivityOptions::with_start_to_close_timeout(Duration::from_millis(cap_ms))
        .retry_policy(
            temporalio_common::RetryPolicy::builder()
                .initial_interval(Duration::from_millis(100))
                .build(),
        )
        .build();
    opts.task_queue = queue;
    opts
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
