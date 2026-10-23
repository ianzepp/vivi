//! The pool job that merges a finished need into main with no model.
//!
//! The pool checks out main at the tip the pump observed, runs one
//! `git merge --no-ff` of the need's tip commit as the harness, and the
//! privileged runner pushes the merge commit to main. The push is
//! fast-forward-only and the runner verifies ancestry, so a main that moved
//! since the observed tip ends `not_ancestor` or `push_refused`: that is the
//! compare-and-swap. A lost race reads as a conflict; retrying belongs to the
//! engine.

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
    /// A real merge conflict, or main moved before the push: the engine
    /// retries or hands the need to the model-driven sync stage.
    Conflict,
    /// The job timed out, was lost, hit a setup error, or left no usable
    /// receipt.
    Failed,
}

/// The no-model job that merges the commit `need_tip` into main. The pool
/// checks main out at `main_tip`; the runner pushes the result only if it
/// still fast-forwards main. None when the project has no merge role.
///
/// The id carries the short `main_tip`, so a retry against a newer main is a
/// distinct job. The job runs on a scratch branch and publishes to main.
#[must_use]
pub fn spec(
    ctx: &Ctx,
    target: &str,
    need: &str,
    need_tip: &str,
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
    let id = format!(
        "merge-main-{step_task}-{}",
        main_tip.get(..12).unwrap_or(main_tip)
    );
    let mut job = JobSpec::new(
        id.clone(),
        "merge",
        role.name.clone(),
        format!("merge into {main} on {target}"),
        format!("Merge the finished work into {main} on {target}.\n"),
    );
    job.command = Some(format!(
        "git merge --no-ff --no-edit -m \"{MESSAGE}\" {need_tip}"
    ));
    job.target = Some(target.to_owned());
    job.from_ref = Some(main_tip.to_owned());
    job.branch = Some(format!(
        "pump/merge-main/{}-{}",
        ref_safe(target),
        ref_safe(&id)
    ));
    job.with = vec![need_tip.to_owned()];
    job.publish_ref = Some(format!("refs/heads/{main}"));
    job.publish_contains = Some(need_tip.to_owned());
    job.budget = budget;
    job.payload = serde_json::json!({
        "need": need,
        "need_tip": need_tip,
        "main_tip": main_tip,
    });
    Some(job)
}

fn ref_safe(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect()
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
        // `git merge` exits non-zero for a conflict and for a setup error such
        // as a missing ref; only the conflict report is a real conflict.
        return if outcome.receipt.contains("CONFLICT") {
            MergeMainResult::Conflict
        } else {
            MergeMainResult::Failed
        };
    }
    if let Some(publish) = outcome.runner_publish()
        && publish.succeeded()
    {
        return MergeMainResult::Merged(publish.remote_sha);
    }
    match publish_state(&outcome.receipt).as_deref() {
        // Main moved since the observed tip: the compare-and-swap lost.
        Some("not_ancestor" | "push_refused") => MergeMainResult::Conflict,
        _ => MergeMainResult::Failed,
    }
}

/// The state word of `runner publish: STATE ...`. A refused publish leaves the
/// trailing sha empty, which `JobOutcome::runner_publish` rejects.
fn publish_state(receipt: &str) -> Option<String> {
    let summary = receipt
        .split_once("hand's final message:")
        .map_or(receipt, |(summary, _)| summary);
    summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("runner publish: "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "merge_main_test.rs"]
mod tests;
