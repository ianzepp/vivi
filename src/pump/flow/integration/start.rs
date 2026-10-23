//! Starts the build flow merge-main and sync steps.
//!
//! Both read three facts before they act: the need branch's target, the need
//! tip (a full SHA) and the target's current main tip. Neither is guessed; a
//! missing one is an error the tick reports.

use crate::error::VivariumError;
use crate::pump::flow::def::FAILED;
use crate::pump::flow::index::StepRow;
use crate::pump::flow::need_branch;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::verify;
use crate::pump::jobs::merge_main;
use crate::pump::need::is_full_sha;

/// The facts a merge-main or sync step starts from.
struct Tips {
    target: String,
    need_tip: String,
    main_tip: String,
    /// The main tip the need branch was cut from; `None` when none is recorded.
    base: Option<String>,
}

fn tips(cx: &StartCx<'_>) -> Result<Tips, VivariumError> {
    let need = &cx.run.need;
    let row = cx
        .store
        .need_branch(need)?
        .ok_or_else(|| VivariumError::Other(format!("need {need} has no need branch to merge")))?;
    let need_tip = need_branch::tip(cx.ctx, cx.store, need)?.ok_or_else(|| {
        VivariumError::Other(format!("need branch of {need} has no tip to merge"))
    })?;
    if !is_full_sha(&need_tip) {
        return Err(VivariumError::Parse(format!(
            "need branch tip of {need} is not a full SHA: {need_tip}"
        )));
    }
    let tctx = cx
        .ctx
        .for_target(&row.target)
        .ok_or_else(|| VivariumError::Config(format!("unknown target {}", row.target)))?;
    let main_tip = verify::remote_tip(&tctx, &tctx.cfg.remote, &tctx.cfg.main)
        .map_err(VivariumError::Other)?
        .ok_or_else(|| {
            VivariumError::Other(format!(
                "main ref {} does not exist on target {}",
                tctx.cfg.main, row.target
            ))
        })?;
    Ok(Tips {
        target: row.target,
        need_tip,
        main_tip,
        base: row.base,
    })
}

/// Why a need branch that never moved has nothing to merge. Also the reason
/// the collector gives, so both name the same facts.
pub(crate) fn nothing_to_merge(tip: &str, base: &str) -> String {
    format!("need tip {tip} == run base {base}: nothing to merge")
}

fn no_merge_role() -> VivariumError {
    VivariumError::Config("expected a merge role in the routing; actual: none".to_owned())
}

/// Starts the no-model merge of the need tip into main.
pub(crate) struct MergeMainStart;

impl StepHandler for MergeMainStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let tips = tips(cx)?;
        if let Some(base) = tips.base.as_deref().filter(|base| *base == tips.need_tip) {
            return Ok(StartAction::Answer {
                word: FAILED.to_owned(),
                payload: serde_json::json!({"reason": nothing_to_merge(&tips.need_tip, base)}),
            });
        }
        let spec = merge_main::spec(
            cx.ctx,
            &tips.target,
            &cx.run.need,
            &tips.need_tip,
            &tips.main_tip,
            &step.task,
        )
        .ok_or_else(no_merge_role)?;
        let mut spec = merge_main::with_attempt(spec, &tips.target, 1);
        spec.task = Some(step.task.clone());
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// Starts the model-driven sync: the merge role merges main into the need
/// branch and publishes the result back to the need branch.
pub(crate) struct SyncStart;

impl StepHandler for SyncStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let role = cx.ctx.routing.merge().ok_or_else(no_merge_role)?;
        let tips = tips(cx)?;
        let need = &cx.run.need;
        let brief = format!(
            "pump-job-kind: sync\n\nMerge main ({main}) into the need branch at {tip}, \
             resolve any conflicts, and publish the result to the need branch. \
             Answer with the merge envelope.\n",
            main = tips.main_tip,
            tip = tips.need_tip,
        );
        let mut spec = role_job(cx.ctx, cx.run, step, "sync", &role.name, &brief);
        spec.target = Some(tips.target.clone());
        spec.from_ref = Some(tips.need_tip);
        spec.branch = Some(format!(
            "pump/merge-main/{}-merge-main-{}-sync",
            merge_main::ref_safe(&tips.target),
            step.task
        ));
        spec.publish_ref = Some(format!("refs/heads/{}", need_branch::branch(need)));
        spec.publish_contains = Some(tips.main_tip.clone());
        spec.with = vec![tips.main_tip];
        Ok(StartAction::Job(Box::new(spec)))
    }
}

#[cfg(test)]
#[path = "start_test.rs"]
mod tests;
