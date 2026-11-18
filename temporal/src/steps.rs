//! Workflow-side step running: activity options per step class, and the
//! failure-class policy from the orchestration-reset README.
//!
//! - Provider (429, quota): the activity fails with type `provider` and a
//!   Retry-After. The workflow sleeps on a durable timer and runs the step
//!   again. That spends no work round and no infra attempt.
//! - Infra (lost host, timeout): Temporal retries the activity up to
//!   `max_infra_attempts`, and the activity resumes from its heartbeat.
//! - Work (red check, change request, ejection): not an error at all; the
//!   activity returns it as a value and the slice loops.
//! - Exhausted: returned as a `ParkCause`, never as a workflow failure.
//! - Placement: a step with host affinity is tried on each host's queue in
//!   turn (`run_placed`). A host that does not pick the task up within its
//!   schedule-to-start wait, or that runs out of infra attempts, hands the
//!   step to the next host; the step resumes from what was pushed.

use std::time::Duration;

use temporalio_common::ActivityDefinition;
use temporalio_common::data_converters::TemporalDeserializable;
use temporalio_common::error::IncomingError;
use temporalio_sdk::{ActivityExecutionError, ActivityOptions, WorkflowContext};

use crate::defs::PROVIDER_FAILURE;
use crate::routing::Placement;
use crate::types::{ParkCause, Tuning};

/// Options for a long agent step: a cap, a heartbeat, infra retries.
pub fn agent_options(tuning: &Tuning) -> ActivityOptions {
    ActivityOptions::with_start_to_close_timeout(Duration::from_millis(tuning.implement_cap_ms))
        .heartbeat_timeout(Duration::from_millis(tuning.heartbeat_ms))
        .retry_policy(infra_retry(tuning))
        .build()
}

/// Options for a short machine or LLM step.
pub fn step_options(tuning: &Tuning) -> ActivityOptions {
    ActivityOptions::with_start_to_close_timeout(Duration::from_millis(tuning.step_cap_ms))
        .retry_policy(infra_retry(tuning))
        .build()
}

fn infra_retry(tuning: &Tuning) -> temporalio_common::RetryPolicy {
    temporalio_common::RetryPolicy::builder()
        .initial_interval(Duration::from_millis(tuning.infra_retry_ms))
        .backoff_coefficient(1.0)
        .maximum_attempts(tuning.max_infra_attempts)
        .build()
}

/// Run one step, waiting out provider throttling without counting it.
/// `activity` makes the activity marker; the macro-generated markers are
/// not `Clone`, and each execution needs one.
pub async fn run_step<W, AD>(
    ctx: &WorkflowContext<W>,
    step: &str,
    activity: impl Fn() -> AD,
    input: AD::Input,
    opts: ActivityOptions,
    tuning: &Tuning,
) -> Result<AD::Output, ParkCause>
where
    AD: ActivityDefinition + 'static,
    AD::Input: Clone,
    AD::Output: TemporalDeserializable,
{
    let mut waits = 0;
    loop {
        let result = ctx
            .execute_activity(activity(), input.clone(), opts.clone())
            .await;
        let err = match result {
            Ok(output) => return Ok(output),
            Err(err) => err,
        };
        let Some(retry_after) = provider_retry_after(&err) else {
            return Err(ParkCause::InfraExhausted {
                step: step.to_string(),
                detail: err.to_string(),
            });
        };
        if waits >= tuning.max_provider_waits {
            return Err(ParkCause::ProviderExhausted {
                step: step.to_string(),
                waits,
            });
        }
        waits += 1;
        let wait = retry_after.unwrap_or(Duration::from_millis(tuning.provider_default_wait_ms));
        ctx.timer(wait).await;
    }
}

/// Run a step on each placement in turn until one host completes it.
/// Returns the output and the host that ran it. A provider failure, or
/// infra failures on every host, end it with the cause.
pub async fn run_placed<W, AD>(
    ctx: &WorkflowContext<W>,
    step: &str,
    activity: impl Fn() -> AD,
    input: AD::Input,
    opts: ActivityOptions,
    tuning: &Tuning,
    placements: &[Placement],
) -> Result<(AD::Output, String), ParkCause>
where
    AD: ActivityDefinition + 'static,
    AD::Input: Clone,
    AD::Output: TemporalDeserializable,
{
    let mut last = ParkCause::InfraExhausted {
        step: step.to_string(),
        detail: "no host serves this step".to_string(),
    };
    for place in placements {
        let mut placed = opts.clone();
        placed.task_queue = Some(place.queue.clone());
        placed.schedule_to_start_timeout = Some(place.wait);
        match run_step(ctx, step, &activity, input.clone(), placed, tuning).await {
            Ok(output) => return Ok((output, place.host.clone())),
            Err(cause @ ParkCause::InfraExhausted { .. }) => last = cause,
            Err(cause) => return Err(cause),
        }
    }
    Err(last)
}

/// `Some(retry_after)` when the activity failed with a provider failure.
fn provider_retry_after(err: &ActivityExecutionError) -> Option<Option<Duration>> {
    match err.cause()? {
        IncomingError::Application(app) if app.type_name() == Some(PROVIDER_FAILURE) => {
            Some(app.next_retry_delay())
        }
        _ => None,
    }
}
