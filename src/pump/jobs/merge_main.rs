//! The pool job that merges a finished need branch into main with no model.
//!
//! The pool checks out main at the tip the pump observed, runs one
//! `git merge --no-ff` as the harness, and the privileged runner publishes the
//! result to main only if main is still at that tip. A compare-and-swap that
//! loses the race reads as a conflict; retrying belongs to the engine.

use super::{JobSpec, JobStatus, PoolJob};
use crate::pump::host::Ctx;

/// Plain commit message for the merge. The runner refuses commits whose
/// messages carry need ids or branch names, so it names neither.
const MESSAGE: &str = "Merge the finished work into the main line";

/// What became of a merge-main job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeMainResult {
    /// Main now holds this commit.
    Merged(String),
    /// The merge or its publish was refused: a content conflict, or main moved.
    Conflict,
    /// The job timed out, was lost, or left no usable receipt.
    Failed,
}

/// The no-model job that merges `need_branch` into main, published only if main
/// is still at `main_tip`. None when the project has no merge role.
#[must_use]
pub fn spec(
    ctx: &Ctx,
    target: &str,
    need: &str,
    need_branch: &str,
    main_tip: &str,
    step_task: &str,
) -> Option<JobSpec> {
    let role = ctx.routing.merge()?;
    let configs = ctx.cfg.target_configs();
    let own = configs.get(target);
    let main = own.map_or(ctx.cfg.main.as_str(), |own| own.main.as_str());
    let budget = own
        .and_then(|own| own.merge.as_ref())
        .map_or(ctx.cfg.merge.remote_limit, |merge| merge.remote_limit);
    let mut job = JobSpec::new(
        format!("merge-main-{step_task}"),
        "merge",
        role.name.clone(),
        format!("merge into {main} on {target}"),
        format!("Merge the finished work into {main} on {target}.\n"),
    );
    job.command = Some(format!(
        "git merge --no-ff --no-edit -m \"{MESSAGE}\" {need_branch}"
    ));
    job.target = Some(target.to_owned());
    job.from_ref = Some(main_tip.to_owned());
    job.with = vec![need_branch.to_owned()];
    job.publish_ref = Some(format!("refs/heads/{main}"));
    job.publish_expect = Some(main_tip.to_owned());
    job.allow_default_branch = true;
    job.budget = budget;
    job.payload = serde_json::json!({ "need": need, "main_tip": main_tip });
    Some(job)
}

/// Read a finished merge-main job. None while it has not reached a terminal
/// pool state.
#[must_use]
pub fn classify(job: &PoolJob) -> Option<MergeMainResult> {
    match job.status {
        JobStatus::Pending | JobStatus::Dispatched => None,
        JobStatus::Timeout | JobStatus::Lost => Some(MergeMainResult::Failed),
        JobStatus::Finished => Some(finished(job)),
    }
}

fn finished(job: &PoolJob) -> MergeMainResult {
    let Some(outcome) = &job.outcome else {
        return MergeMainResult::Failed;
    };
    if matches!(outcome.state.as_str(), "timeout" | "lost") {
        return MergeMainResult::Failed;
    }
    if outcome.agent_success() == Some(false) {
        return MergeMainResult::Conflict;
    }
    match outcome.runner_publish() {
        Some(publish) if publish.succeeded() => MergeMainResult::Merged(publish.remote_sha),
        Some(_) => MergeMainResult::Conflict,
        None => MergeMainResult::Failed,
    }
}

#[cfg(test)]
#[path = "merge_main_test.rs"]
mod tests;
