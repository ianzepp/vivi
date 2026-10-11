//! `dispatch`: plan a job, claim a slot, deliver the job as one upload, start
//! the slot. The caller records the dispatch on the task afterwards.

use std::time::Instant;

use sha2::{Digest, Sha256};

use super::claim::{Claim, claim_slot, unclaim};
use super::collect::Pacing;
use super::config::PoolConfig;
use super::github::Secret;
use super::job::{self, JOB_ARCHIVE, JobPlan, build_bundle};
use super::provider::PoolProvider;
use super::record::DispatchRecord;
use crate::error::VivariumError;

/// Budget when `--budget` is not given.
pub const DEFAULT_BUDGET_SECS: u64 = 1800;
/// Bounds for `--budget`.
pub const BUDGET_RANGE: std::ops::RangeInclusive<u64> = 1..=43_200;

/// The flags of `vivi pool dispatch`.
#[derive(Debug, Clone, Default)]
pub struct DispatchFlags {
    pub model: String,
    pub from: Option<String>,
    pub branch: Option<String>,
    pub budget: Option<u64>,
    /// Allow pushing to the repository's default branch.
    pub allow_default_branch: bool,
}

/// A fresh job id: twelve hex digits.
#[must_use]
pub fn new_job_id(task: &str, nanos: u128, pid: u32) -> String {
    let digest = Sha256::digest(format!("{task}\0{nanos}\0{pid}"));
    hex::encode(digest)[..12].to_string()
}

/// Validate everything that can be checked without touching a machine and
/// assemble the job. `rules` is the standing-rules text to deliver.
///
/// # Errors
/// Returns an error if the harness or committer is not configured, the model
/// is not allowed, `from`/`branch` are missing or malformed, or the budget is
/// out of range.
pub fn plan_job(
    config: &PoolConfig,
    task: (&str, &str, &str),
    flags: &DispatchFlags,
    rules: String,
    job: String,
) -> Result<JobPlan, VivariumError> {
    let (handle, subject, body) = task;
    let harness = config
        .harness
        .as_ref()
        .ok_or_else(|| VivariumError::Config("[pool.harness] is required to dispatch".into()))?;
    let committer = config.committer.clone().ok_or_else(|| {
        VivariumError::Config("[pool.committer] (name, email) is required to dispatch".into())
    })?;
    job::check_model(&flags.model, &harness.models)?;
    let refs = job::resolve_refs(flags.from.as_deref(), flags.branch.as_deref(), body)?;
    let budget = flags.budget.unwrap_or(DEFAULT_BUDGET_SECS);
    if !BUDGET_RANGE.contains(&budget) {
        return Err(VivariumError::Message(format!(
            "budget must be between {} and {} seconds",
            BUDGET_RANGE.start(),
            BUDGET_RANGE.end()
        )));
    }
    Ok(JobPlan {
        job,
        remote: config.remote_address(),
        task: handle.to_string(),
        subject: subject.to_string(),
        brief: body.to_string(),
        rules,
        refs,
        model: flags.model.clone(),
        budget,
        depth: 1,
        harness_cmd: job::render_harness(&harness.run, &flags.model),
        env_names: harness.env.clone(),
        committer,
    })
}

/// Refuse a push target that is the repository's default branch, unless the
/// caller said it is wanted.
///
/// # Errors
/// Returns an error naming the branch and the override flag.
pub fn check_push_target(
    branch: &str,
    default_branch: &str,
    allow: bool,
) -> Result<(), VivariumError> {
    if branch == default_branch && !allow {
        return Err(VivariumError::Message(format!(
            "refusing to push to the default branch \"{default_branch}\"; pass --allow-default-branch if that is intended"
        )));
    }
    Ok(())
}

/// Claim a free slot and start `plan` on it. `start` is the commit `from`
/// resolved to, recorded with the dispatch.
///
/// # Errors
/// Returns an error if no slot is free or the provider fails; a half-made
/// claim is undone before the error returns.
pub fn dispatch(
    provider: &dyn PoolProvider,
    pool: &str,
    plan: &JobPlan,
    token: &Secret,
    start: &str,
    now: u64,
    pacing: &Pacing,
) -> Result<DispatchRecord, VivariumError> {
    let claim = claim_slot(provider, pool, &plan.task, now)?;
    if let Err(e) = deliver(provider, &claim, plan, token, pacing) {
        if let Err(undo) = unclaim(provider, &claim) {
            eprintln!("warning: could not fully undo the claim: {undo}");
        }
        return Err(e);
    }
    Ok(DispatchRecord {
        slot: claim.slot.name,
        machine: claim.slot.id,
        job: plan.job.clone(),
        model: plan.model.clone(),
        from: plan.refs.from.clone(),
        branch: plan.refs.branch.clone(),
        start: start.to_string(),
        budget: plan.budget,
        at: now,
    })
}

fn deliver(
    provider: &dyn PoolProvider,
    claim: &Claim,
    plan: &JobPlan,
    token: &Secret,
    pacing: &Pacing,
) -> Result<(), VivariumError> {
    let id = &claim.slot.id;
    let archive = job::pack(&build_bundle(plan, &claim.slot.name, token))?;
    provider.put_file(id, &claim.lease, JOB_ARCHIVE, &archive)?;
    provider.release(id, &claim.lease)?;
    wait_stopped(provider, id, pacing)?;
    provider.start(id)
}

/// A config update replaces the machine instance; wait for it to settle back
/// to `stopped` before starting it.
fn wait_stopped(
    provider: &dyn PoolProvider,
    id: &str,
    pacing: &Pacing,
) -> Result<(), VivariumError> {
    let deadline = Instant::now() + pacing.ready_timeout;
    loop {
        if provider.machine(id)?.state == "stopped" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(VivariumError::Message(format!(
                "slot {id} did not settle after the job was delivered"
            )));
        }
        std::thread::sleep(pacing.poll);
    }
}

#[cfg(test)]
#[path = "dispatch_test.rs"]
mod tests;
