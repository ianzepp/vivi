//! Rung steps: start rung jobs at the need-branch tip and collect them, with
//! one retry when the job is lost or times out.

use crate::error::VivariumError;
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::decide;
use crate::pump::flow::def::{FAILED, FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::need_branch;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::host::Ctx;
use crate::pump::jobs::rung_result::EnvFailure;
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob, rung, rung_result};

/// The rung a need-rung step runs.
const NEED_RUNG: &str = "need";
/// Attempts a rung step makes before a lost or timed-out job answers `failed`.
const MAX_ATTEMPTS: u32 = 2;
/// The class of the audit step whose payload may name a raised rung.

/// The rung job for `step`: [`rung::spec`] bound to the step task. N6 and N7
/// call this for rung-main (main tip, repair branch tip).
///
/// # Errors
/// Returns [`VivariumError::Config`] naming the rung and target when the rung
/// is not configured.
pub(crate) fn job(
    ctx: &Ctx,
    target: &str,
    rung: &str,
    tip: &str,
    step: &StepRow,
    attempt: u32,
) -> Result<JobSpec, VivariumError> {
    let mut spec = rung::spec(ctx, target, rung, tip, &step.task, attempt).ok_or_else(|| {
        VivariumError::Config(format!(
            "rung `{rung}`: expected a command in [pump.rungs] for target {target}; actual: not configured"
        ))
    })?;
    spec.task = Some(step.task.clone());
    Ok(spec)
}

/// Starts a need-rung step at the need-branch tip.
struct RungStart;

/// Starts a rung-raised step: the sticky raise of the run (see `decide::sticky_raise`), else `need`.
struct RaisedStart;

impl StepHandler for RungStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        start_rung(cx, step, NEED_RUNG)
    }
}

impl StepHandler for RaisedStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let raised = decide::sticky_raise(cx.store, &cx.run.need, cx.run.run)?;
        start_rung(cx, step, raised.as_deref().unwrap_or(NEED_RUNG))
    }
}

/// The rung job for `rung` at the tip of the need branch.
fn start_rung(cx: &StartCx<'_>, step: &StepRow, rung: &str) -> Result<StartAction, VivariumError> {
    let need = &cx.run.need;
    let row = cx.store.need_branch(need)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "rung {rung} for need {need}: expected a need_branches row; actual: none"
        ))
    })?;
    let tip = need_branch::tip(cx.ctx, cx.store, need)?.ok_or_else(|| {
        VivariumError::Other(format!(
            "rung {rung} for need {need}: expected a need-branch tip; actual: none"
        ))
    })?;
    let spec = job(cx.ctx, &row.target, rung, &tip, step, 1)?;
    Ok(StartAction::Job(Box::new(spec)))
}

/// Collects a rung job: the exit status decides `pass` or `fail`; a lost or
/// timed-out job runs once more.
pub(crate) struct RungCollector;

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for RungCollector {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        pool_job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        _class: &NodeClass,
    ) -> Result<(), VivariumError> {
        if let Some(env) = rung_result::env_failure(pool_job) {
            return hold_env(cx, pool_job, step, &env);
        }
        let lost = matches!(pool_job.status, JobStatus::Lost | JobStatus::Timeout);
        if lost && row.attempt < MAX_ATTEMPTS {
            return retry(cx, pool_job, row, step);
        }
        let (word, payload) = answer(pool_job);
        cx.advance_word(step, word, payload)
    }
}

/// The worker could not run the rung script: the job says nothing about the
/// code. Alarm once, queue nothing, and hold the step for the operator. The
/// rung's lost-retry cap and the flow attempt are untouched.
pub(crate) fn hold_env<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
    env: &EnvFailure,
) -> Result<(), VivariumError> {
    let rung = job
        .payload
        .get("rung")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none");
    raise_once(
        cx.ctx,
        rung_result::ENV_FAILURE,
        &job.id,
        &format!(
            "rung {rung} for step {} task {} on worker {}: expected the rung script to run; \
             actual: {:?}, tool {}: {}; answer with vivi pump answer {} pass|fail|failed",
            step.node,
            step.task,
            job.pool.as_deref().unwrap_or("none"),
            env.kind,
            env.tool.as_deref().unwrap_or("none"),
            env.evidence,
            step.task
        ),
    );
    cx.store
        .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
}

/// The answer word and outcome payload of a finished rung job. The payload
/// carries `exit`, `tail` and the sorted `failing` ids: facts from cargo's own
/// output (N4a-29), never from model text.
pub(crate) fn answer(pool_job: &PoolJob) -> (&'static str, serde_json::Value) {
    let result = rung_result::parse(pool_job);
    let word = match (result.pass, result.exit) {
        (true, _) => "pass",
        (false, Some(_)) => "fail",
        (false, None) => FAILED,
    };
    let payload = serde_json::json!({
        "exit": result.exit,
        "tail": result.tail,
        "failing": result.failing,
    });
    (word, payload)
}

fn payload_str<'a>(job: &'a PoolJob, key: &str) -> Result<&'a str, VivariumError> {
    job.payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| VivariumError::Parse(format!("rung job {} has no `{key}` payload", job.id)))
}

/// Enqueue the next attempt of a lost rung job at the same tip.
fn retry<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    lost: &PoolJob,
    row: &StepJobRow,
    step: &StepRow,
) -> Result<(), VivariumError> {
    let attempt = row.attempt + 1;
    let target = payload_str(lost, "target")?;
    let rung = payload_str(lost, "rung")?;
    let tip = payload_str(lost, "tip")?;
    let mut spec = job(cx.ctx, target, rung, tip, step, attempt)?;
    let mut payload = lost.payload.clone();
    if let Some(map) = payload.as_object_mut() {
        map.insert("attempt".to_owned(), serde_json::json!(attempt));
    }
    spec.payload = payload;
    let id = spec.id.clone();
    jobs::enqueue(cx.st, spec)?;
    cx.store.insert_step_job(
        &cx.run.need,
        &StepJobRow {
            job: id,
            run: row.run,
            node: row.node.clone(),
            task: row.task.clone(),
            attempt,
        },
    )?;
    cx.store
        .set_step_state(&step.task, StepState::Running, None)
}

/// Register the build flow's rung handlers and the collector of every rung class.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(FlowKind::Build, NodeClass::RungNeed, Box::new(RungStart));
    registry.handler(
        FlowKind::Build,
        NodeClass::RungRaised,
        Box::new(RaisedStart),
    );
    for class in [
        NodeClass::RungNeed,
        NodeClass::RungMain,
        NodeClass::RungRelease,
        NodeClass::RungRaised,
    ] {
        registry.collector(FlowKind::Build, class, Box::new(RungCollector));
    }
}

#[cfg(test)]
#[path = "rung_step_test.rs"]
mod tests;
