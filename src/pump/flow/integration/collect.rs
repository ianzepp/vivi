//! Collects the build flow merge-main step.
//!
//! A merge that lost the compare-and-swap because main moved is queued again
//! at the new main tip, up to [`MOVED_RETRIES`] times; a real merge conflict
//! answers `conflict` at once. A lost or timed-out job is settled by git, never
//! by the pool's say-so.

use crate::error::VivariumError;
use crate::pump::flow::collect_step::{RunCx, StepCollector, merged_payload};
use crate::pump::flow::def::{FAILED, NodeClass};
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::integration::start::nothing_to_merge;
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR, raise_once};
use crate::pump::flow::verify;
use crate::pump::jobs::merge_main::{self, MergeMainResult};
use crate::pump::jobs::rung_result::{self, EnvFailure};
use crate::pump::jobs::{self, JobOutcome, JobStatus, JobStore, PoolJob};

/// Times a merge-main step is queued again after main moved or a lost job left
/// main unchanged; the next failure answers.
const MOVED_RETRIES: u32 = 3;

/// The build flow's merge-main collector.
pub(crate) struct BuildMergeMain;

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for BuildMergeMain {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        _class: &NodeClass,
    ) -> Result<(), VivariumError> {
        if let Some(env) = rung_result::env_failure(job) {
            return hold_env(cx, job, step, &env);
        }
        match merge_main::classify(job) {
            None => Ok(()),
            Some(MergeMainResult::Merged(sha)) => {
                cx.advance_word(step, "merged", merged_payload(&sha))
            }
            Some(MergeMainResult::Conflict) if merge_main::main_moved(job) => {
                if row.attempt <= MOVED_RETRIES {
                    requeue(cx, job, row, step)
                } else {
                    conflict(cx, job, step)
                }
            }
            Some(MergeMainResult::Conflict) => conflict(cx, job, step),
            Some(MergeMainResult::Failed) if lost(job) => settle_lost(cx, job, row, step),
            Some(MergeMainResult::Failed) => failed(cx, job, step),
        }
    }
}

/// The worker could not run git or its checkout failed: the job says nothing
/// about main. Alarm once, queue nothing, and hold the step for the operator.
fn hold_env<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
    env: &EnvFailure,
) -> Result<(), VivariumError> {
    raise_once(
        cx.ctx,
        rung_result::ENV_FAILURE,
        &job.id,
        &format!(
            "merge-main job {} for step {} ({:?}, tool {}) on target {} pool {}: {}; \
             answer with `vivi pump answer` (merged, conflict or failed)",
            job.id,
            step.task,
            env.kind,
            env.tool.as_deref().unwrap_or("none"),
            job.target.as_deref().unwrap_or("none"),
            job.pool.as_deref().unwrap_or("none"),
            env.evidence
        ),
    );
    cx.store
        .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
}

fn conflict<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
) -> Result<(), VivariumError> {
    cx.advance_word(step, "conflict", serde_json::json!({"job": job.id}))
}

fn failed<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
) -> Result<(), VivariumError> {
    let need_tip = payload_str(job, "need_tip")?;
    let main_tip = payload_str(job, "main_tip")?;
    let status = format!("{:?}", job.status).to_lowercase();
    let publish = job
        .outcome
        .as_ref()
        .and_then(JobOutcome::runner_publish)
        .map_or_else(|| "none".to_owned(), |receipt| receipt.state);
    let reason = format!(
        "expected main to take {need_tip}; actual: merge-main {status} with publish {publish}"
    );
    cx.advance_word(
        step,
        FAILED,
        serde_json::json!({
            "job": job.id,
            "reason": reason,
            "need_tip": need_tip,
            "main_tip": main_tip,
        }),
    )
}

/// A job the pool lost or timed out: no receipt says what happened to main.
fn lost(job: &PoolJob) -> bool {
    matches!(job.status, JobStatus::Lost | JobStatus::Timeout)
        || job
            .outcome
            .as_ref()
            .is_some_and(|outcome| matches!(outcome.state.as_str(), "timeout" | "lost"))
}

/// Git decides what a lost merge did to main.
fn settle_lost<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    row: &StepJobRow,
    step: &StepRow,
) -> Result<(), VivariumError> {
    let remote_ref = job.publish_ref.clone().unwrap_or_default();
    let target = job.target.clone().unwrap_or_default();
    let need_tip = payload_str(job, "need_tip")?;
    let base = cx.store.need_branch(&cx.run.need)?.and_then(|row| row.base);
    if let Some(base) = base.as_deref().filter(|base| *base == need_tip) {
        return cx.advance_word(
            step,
            FAILED,
            serde_json::json!({"job": job.id, "reason": nothing_to_merge(need_tip, base)}),
        );
    }
    // The push is fast-forward only: a main that moved without the need tip
    // means the merge did not land.
    match verify::ref_contains(cx.ctx, &target, &remote_ref, need_tip) {
        Ok(Some(tip)) => cx.advance_word(step, "merged", merged_payload(&tip)),
        Ok(None) if row.attempt <= MOVED_RETRIES => requeue(cx, job, row, step),
        Ok(None) => failed(cx, job, step),
        Err(reason) => {
            cx.ctx.exception(
                SIDE_EFFECT_UNCLEAR,
                &step.task,
                &format!(
                    "step {} task {} job {} ref {remote_ref}: {reason}; answer with `vivi pump answer`",
                    step.node, step.task, job.id
                ),
            );
            cx.store
                .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
        }
    }
}

fn payload_str<'a>(job: &'a PoolJob, key: &str) -> Result<&'a str, VivariumError> {
    job.payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            VivariumError::Parse(format!("merge-main job {} has no `{key}` payload", job.id))
        })
}

/// Queue the merge again as the next attempt, at main's current tip.
fn requeue<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    old: &PoolJob,
    row: &StepJobRow,
    step: &StepRow,
) -> Result<(), VivariumError> {
    let attempt = row.attempt + 1;
    let target = old
        .target
        .as_deref()
        .ok_or_else(|| VivariumError::Parse(format!("merge-main job {} has no target", old.id)))?;
    let need_tip = payload_str(old, "need_tip")?;
    let tctx = cx
        .ctx
        .for_target(target)
        .ok_or_else(|| VivariumError::Config(format!("unknown target {target}")))?;
    let main_tip = verify::remote_tip(&tctx, &tctx.cfg.remote, &tctx.cfg.main)
        .map_err(VivariumError::Other)?
        .ok_or_else(|| VivariumError::Other(format!("main of target {target} has no tip")))?;
    let spec = merge_main::spec(
        cx.ctx,
        target,
        &cx.run.need,
        need_tip,
        &main_tip,
        &step.task,
    )
    .ok_or_else(|| VivariumError::Config(String::from("no merge role for the main merge")))?;
    let mut spec = merge_main::with_attempt(spec, target, attempt);
    spec.task.clone_from(&old.task);
    let mut payload = old.payload.clone();
    if let Some(map) = payload.as_object_mut() {
        map.insert("attempt".to_owned(), serde_json::json!(attempt));
        map.insert("main_tip".to_owned(), serde_json::json!(main_tip));
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

#[cfg(test)]
#[path = "collect_test.rs"]
mod tests;
