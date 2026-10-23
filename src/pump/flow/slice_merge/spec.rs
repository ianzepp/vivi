//! Builds the model-free slice merge job.
//!
//! The job starts from the need-branch tip (the run's base while the branch
//! does not exist yet), merges the hand commit, runs the formatter and the
//! `task` rung, and the runner publishes the need branch only when the whole
//! command exits 0.

use crate::error::VivariumError;
use crate::pump::config::DEFAULT_RUNG_TIMEOUT;
use crate::pump::flow::fanout;
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::need_branch;
use crate::pump::flow::slice_dispatch::{self, SliceMail};
use crate::pump::flow::verify::remote_tip;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Job kind of a slice merge job.
pub(crate) const KIND: &str = "slice-merge";

/// Plain commit message for the merge. The runner refuses messages that carry
/// need ids or branch names, so it names neither.
const MESSAGE: &str = "Merge one slice onto the need branch";

/// The newest terminal hand job that ran `task`: highest payload `attempt`,
/// then latest `queued_at`.
pub(crate) fn hand_job_of<S: JobStore>(st: &S, task: &str) -> Option<PoolJob> {
    st.jobs()
        .values()
        .filter(|job| {
            job.kind == slice_dispatch::KIND
                && job.status.is_terminal()
                && job.task.as_deref() == Some(task)
        })
        .max_by_key(|job| (attempt_of(job), job.queued_at))
        .cloned()
}

fn attempt_of(job: &PoolJob) -> u64 {
    job.payload
        .get("attempt")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
}

/// The commit a hand job published: the runner's publish receipt when it
/// succeeded, else the sha of the receipt summary's `push: pushed <branch>
/// <sha>` line.
pub(crate) fn hand_commit(job: &PoolJob) -> Option<String> {
    let outcome = job.outcome.as_ref()?;
    if let Some(publish) = outcome.runner_publish()
        && publish.succeeded()
    {
        return Some(publish.remote_sha);
    }
    let summary = outcome
        .receipt
        .split("\n\n--- ")
        .next()
        .and_then(|head| head.split("hand's final message:").next())
        .unwrap_or_default();
    let line = summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("push: "))?;
    let mut words = line.split_whitespace();
    (words.next()? == "pushed").then_some(())?;
    words.next()?;
    words.next().map(str::to_owned)
}

/// The job that merges the hand commit of `slice` onto the need branch.
///
/// # Errors
/// Returns [`VivariumError`] for a slice task with no `target:` line, an
/// unknown target, a project with no merge role, an unconfigured `task` rung,
/// a hand job with no published commit or branch, or a failed ls-remote.
///
/// `try_no` is the 1-based merge try of this hand job; it keeps the job id and
/// the scratch branch unique per try.
pub(crate) fn spec<M: SliceMail + ?Sized>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
    slice: &StepRow,
    hand: &PoolJob,
    try_no: u32,
) -> Result<JobSpec, VivariumError> {
    let body = mail.task_body(&slice.task)?;
    let target = fanout::target_of(&body).ok_or_else(|| {
        VivariumError::Parse(format!("slice task {} names no target", slice.task))
    })?;
    let tctx = ctx
        .for_target(target)
        .ok_or_else(|| VivariumError::Config(format!("unknown target {target}")))?;
    let sha = hand_commit(hand).ok_or_else(|| {
        VivariumError::Other(format!("hand job {} published no commit to merge", hand.id))
    })?;
    let hand_branch = hand
        .branch
        .as_deref()
        .ok_or_else(|| VivariumError::Other(format!("hand job {} has no branch", hand.id)))?;
    let role = ctx
        .routing
        .merge()
        .ok_or_else(|| VivariumError::Config(String::from("no merge role is configured")))?;
    let rungs = ctx.cfg.effective_rungs(Some(target));
    let task = rungs.command("task").ok_or_else(|| {
        VivariumError::Config(format!("rung task is not configured for target {target}"))
    })?;
    let base = need_branch::record_base(ctx, store, &run.need, run.run, target)?;
    let need_ref = format!("refs/heads/{}", need_branch::branch(&run.need));
    let before = remote_tip(&tctx, &tctx.cfg.remote, &need_ref).map_err(VivariumError::Other)?;
    let from_ref = before.clone().unwrap_or(base);
    let mut command = format!("git merge --no-ff --no-edit -m \"{MESSAGE}\" {sha}");
    let format = tctx.cfg.merge.format.trim();
    if !format.is_empty() {
        command = format!("{command} && {format}");
    }
    command = format!("{command} && {task}");
    let mut job = JobSpec::new(
        format!("{}-merge-t{try_no}", hand.id),
        KIND,
        role.name.clone(),
        format!("merge {} of {} onto the need branch", slice.node, run.need),
        format!(
            "Merge the hand commit {sha} of {} onto the need branch of {} on {target}, \
             format it, and run the task rung. The need branch moves only when all of it passes.\n\n$ {command}\n",
            slice.node, run.need
        ),
    );
    job.command = Some(command);
    job.target = Some(target.to_owned());
    job.from_ref = Some(from_ref);
    job.with = vec![sha.clone()];
    job.branch = Some(format!("{hand_branch}-merge-t{try_no}"));
    job.publish_ref = Some(need_ref);
    job.publish_contains = Some(sha.clone());
    job.pool_candidates = rungs.pools.clone();
    job.budget = rungs.timeout.unwrap_or(DEFAULT_RUNG_TIMEOUT) + tctx.cfg.merge.remote_limit;
    job.task = Some(slice.task.clone());
    job.payload = serde_json::json!({
        "need": run.need,
        "run": run.run,
        "node": slice.node,
        "task": slice.task,
        "attempt": attempt_of(hand),
        "hand_job": hand.id,
        "try": try_no,
        "hand_sha": sha,
        "target": target,
        "before": before,
        "kind": run.kind,
        "class": "slice-merge",
    });
    Ok(job)
}

#[cfg(test)]
#[path = "spec_test.rs"]
mod tests;
