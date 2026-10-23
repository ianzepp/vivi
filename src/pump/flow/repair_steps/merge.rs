//! The repair flow's merge-main collector.
//!
//! The repair merge is cut from the attempt's base, so a moved main is a
//! conflict at once and nothing is queued again. A job with no decisive
//! receipt (failed, lost, timed out) is settled by git: main either contains
//! the commit or it does not. Only a git that cannot answer holds the step.

use crate::error::VivariumError;
use crate::pump::flow::collect_step::{RunCx, StepCollector, merged_payload};
use crate::pump::flow::def::{FAILED, NodeClass};
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::main_rung;
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR, raise_once};
use crate::pump::flow::verify;
use crate::pump::jobs::merge_main::{self, MergeMainResult};
use crate::pump::jobs::rung_result::{self, EnvFailure};
use crate::pump::jobs::{JobStore, PoolJob};

/// The repair flow's merge-main collector.
pub(super) struct RepairMergeMain;

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for RepairMergeMain {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        job: &PoolJob,
        _row: &StepJobRow,
        step: &StepRow,
        _class: &NodeClass,
    ) -> Result<(), VivariumError> {
        if let Some(env) = rung_result::env_failure(job) {
            return hold_env(cx, job, step, &env);
        }
        match merge_main::classify(job) {
            None => Ok(()),
            Some(MergeMainResult::Merged(sha)) => land(cx, job, step, &sha, &sha),
            Some(MergeMainResult::Conflict) => {
                let reason = if merge_main::main_moved(job) {
                    "main moved"
                } else {
                    "merge conflict"
                };
                answer(cx, job, step, "conflict", reason)
            }
            Some(MergeMainResult::Failed) => settle(cx, job, step),
        }
    }
}

/// Record `recorded` as the repair commit for the job's target, then answer
/// merged with `sha` (the main commit that landed it).
fn land<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
    sha: &str,
    recorded: &str,
) -> Result<(), VivariumError> {
    let target = job
        .target
        .as_deref()
        .ok_or_else(|| VivariumError::Parse(format!("merge-main job {} has no target", job.id)))?;
    cx.store
        .set_main_state(&main_rung::key(main_rung::REPAIRED, target), recorded)?;
    cx.advance_word(step, "merged", merged_payload(cx.ctx, job, sha))
}

/// The worker could not run git: the job says nothing about main. Alarm once
/// and hold the step for the operator; the attempt is not counted.
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
            "repair merge-main job {} for step {}: expected git to run on the worker; \
             actual: {:?} tool {}: {}; answer with vivi pump answer {} merged|conflict|failed",
            job.id,
            step.task,
            env.kind,
            env.tool.as_deref().unwrap_or("none"),
            env.evidence,
            step.task
        ),
    );
    hold(cx, step)
}

fn hold<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    step: &StepRow,
) -> Result<(), VivariumError> {
    cx.store
        .set_step_state(&step.task, StepState::Waiting, Some(OPERATOR_HOLD))
}

fn answer<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
    word: &str,
    reason: &str,
) -> Result<(), VivariumError> {
    cx.advance_word(
        step,
        word,
        serde_json::json!({"job": job.id, "reason": reason}),
    )
}

fn payload_str<'a>(job: &'a PoolJob, key: &str) -> Result<&'a str, VivariumError> {
    job.payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            VivariumError::Parse(format!("merge-main job {} has no `{key}` payload", job.id))
        })
}

/// Git decides what a job without a decisive receipt did to main.
fn settle<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
) -> Result<(), VivariumError> {
    let publish_ref = job.publish_ref.clone().unwrap_or_default();
    let target = job.target.clone().unwrap_or_default();
    let need_tip = payload_str(job, "need_tip")?;
    let base = payload_str(job, "main_tip")?;
    match verify::ref_contains(cx.ctx, &target, &publish_ref, need_tip) {
        // Main holds the repair, but its tip may be a later commit: record the
        // repair commit itself, which every later main holding the repair contains.
        Ok(Some(tip)) => land(cx, job, step, &tip, need_tip),
        Ok(None) => match main_now(cx, &target) {
            Ok(now) if now != base => {
                let reason = format!("main moved from {base} to {now} without {need_tip}");
                answer(cx, job, step, "conflict", &reason)
            }
            Ok(now) => {
                let reason =
                    format!("expected main to contain {need_tip}; actual: main {now} without it");
                answer(cx, job, step, FAILED, &reason)
            }
            Err(reason) => unclear(cx, job, step, &publish_ref, &reason),
        },
        Err(reason) => unclear(cx, job, step, &publish_ref, &reason),
    }
}

/// The current tip of the target's main.
fn main_now<S, M>(cx: &RunCx<'_, S, M>, target: &str) -> Result<String, String> {
    let tctx = cx
        .ctx
        .for_target(target)
        .ok_or_else(|| format!("unknown target {target}"))?;
    verify::remote_tip(&tctx, &tctx.cfg.remote, &tctx.cfg.main)?
        .ok_or_else(|| format!("main of target {target} has no tip"))
}

/// Git could not answer: the one case that holds a settled-by-git step.
fn unclear<S: JobStore, M: FlowMail>(
    cx: &mut RunCx<'_, S, M>,
    job: &PoolJob,
    step: &StepRow,
    publish_ref: &str,
    reason: &str,
) -> Result<(), VivariumError> {
    raise_once(
        cx.ctx,
        SIDE_EFFECT_UNCLEAR,
        &job.id,
        &format!(
            "step {} task {} job {} ref {publish_ref}: {reason}; answer with `vivi pump answer`",
            step.node, step.task, job.id
        ),
    );
    hold(cx, step)
}

#[cfg(test)]
#[path = "merge_test.rs"]
mod tests;
