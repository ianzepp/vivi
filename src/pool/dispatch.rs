//! `dispatch`: plan a job, claim a slot, deliver the job as one upload, start
//! the slot. The caller records the dispatch on the task afterwards.

use std::time::Instant;

use sha2::{Digest, Sha256};

use super::claim::{Claim, claim_slot, open_slot, unclaim};
use super::collect::Pacing;
use super::config::PoolConfig;
use super::github::Secret;
use super::job::{self, JOB_ARCHIVE, JobPlan, build_bundle};
use super::progress::Progress;
use super::provider::{OpenRequest, PoolProvider, SlotLifetime};
use super::record::DispatchRecord;
use super::run::dispatch_spec;
use super::slot::PoolId;
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
    /// The job publishes nothing: detached checkout, no commit, no push, a
    /// token that cannot push. Excludes `branch`.
    pub read_only: bool,
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
    pool: &str,
    task: (&str, &str, &str),
    flags: &DispatchFlags,
    rules: String,
    job: String,
) -> Result<JobPlan, VivariumError> {
    let (handle, subject, body) = task;
    if config.embedded.is_none() && config.worktree_source().is_none() {
        config.require_repo(pool, "dispatch")?;
    }
    let harness = config.harness.as_ref().ok_or_else(|| {
        VivariumError::Config(format!("[pools.{pool}.harness] is required to dispatch"))
    })?;
    let committer = config.committer.clone().ok_or_else(|| {
        VivariumError::Config(format!(
            "[pools.{pool}.committer] (name, email) is required to dispatch"
        ))
    })?;
    job::check_model(&flags.model, &harness.models)?;
    let refs = plan_refs(flags, body)?;
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
        checkout: None,
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
        read_only: flags.read_only,
        facts: config.facts.clone(),
    })
}

/// `from` and `branch` for the flags. A read-only job names no branch.
fn plan_refs(flags: &DispatchFlags, body: &str) -> Result<job::Refs, VivariumError> {
    if !flags.read_only {
        return job::resolve_refs(flags.from.as_deref(), flags.branch.as_deref(), body);
    }
    if flags.branch.is_some() {
        return Err(VivariumError::Message(
            "--branch cannot be used with --read-only: a read-only job has no branch".into(),
        ));
    }
    Ok(job::Refs {
        from: job::resolve_from(flags.from.as_deref(), body)?,
        branch: String::new(),
    })
}

/// The `from` the job carries: `resolved` when `requested` is an abbreviated
/// hash (4 to 39 hexadecimal characters, case-insensitive) whose commit
/// `resolved` begins with, `requested` unchanged otherwise (a branch, a tag,
/// a full hash). A Git server cannot resolve an abbreviated hash during
/// `git fetch`, so the job must be handed the whole thing.
#[must_use]
pub fn full_from(requested: &str, resolved: &str) -> String {
    let abbreviated = (4..40).contains(&requested.len())
        && requested.chars().all(|c| c.is_ascii_hexdigit())
        && resolved
            .get(..requested.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(requested));
    if abbreviated {
        resolved.to_string()
    } else {
        requested.to_string()
    }
}

/// What a read-only dispatch cannot promise on this pool, if anything: the
/// confined providers (Docker, Fly) hold no credentials, so there is nothing
/// to say there.
#[must_use]
pub fn read_only_warning(config: &PoolConfig) -> Option<String> {
    if config.worktree_source().is_some() {
        return Some(
            "warning: --read-only in a worktree pool: the job makes no commit and no branch, \
             and there is no token, but the worker shares the repository and could still \
             commit to it with plain Git; Vivi cannot stop that"
                .into(),
        );
    }
    matches!(config.provider.as_str(), "ssh" | "local").then(|| {
        format!(
            "warning: --read-only on the {} provider: the runner publishes nothing and the \
             token cannot push, but the worker runs as a real user account that may hold its \
             own credentials, and Vivi cannot stop it from using that account's own access",
            config.provider
        )
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

/// Check `--ready-timeout` before anything is rented or any token minted.
///
/// # Errors
/// Returns a usage error if the pool's slots rest between jobs (there is no
/// pod to wait for), or the seconds are out of range.
pub fn check_ready_timeout(
    lifetime: SlotLifetime,
    secs: Option<u64>,
) -> Result<Option<u64>, VivariumError> {
    let Some(secs) = secs else {
        return Ok(None);
    };
    if lifetime != SlotLifetime::PerDispatch {
        return Err(VivariumError::Message(
            "--ready-timeout applies only to a pool whose slots are made per dispatch (a RunPod pool); this pool keeps resting slots, so there is no new machine to wait for".into(),
        ));
    }
    super::runpod_config::check_ready_timeout(secs, "--ready-timeout")
        .map_err(VivariumError::Message)?;
    Ok(Some(secs))
}

/// What a dispatch starts: the job, the token that lets it fetch (and push),
/// the commit it starts from, and the pool's settings and progress sink.
pub struct Launch<'a> {
    pub config: &'a PoolConfig,
    pub plan: &'a JobPlan,
    pub token: &'a Secret,
    /// The commit `from` resolved to, recorded with the dispatch.
    pub start: &'a str,
    pub progress: &'a Progress,
    /// `--ready-timeout`: seconds a new pod may take to answer on SSH for
    /// this dispatch (none: the pool's setting).
    pub ready_timeout: Option<u64>,
}

/// Get a slot for `launch` and start its job on it: claim a free resting slot,
/// or have a per-dispatch host make one.
///
/// # Errors
/// Returns an error if no slot is free or the provider fails; a half-made
/// claim is undone before the error returns.
pub fn dispatch(
    provider: &dyn PoolProvider,
    id: &PoolId,
    launch: &Launch,
    now: u64,
    pacing: &Pacing,
) -> Result<DispatchRecord, VivariumError> {
    let plan = launch.plan;
    let claim = take_slot(provider, id, launch, now)?;
    if let Err(e) = deliver(provider, &claim, plan, launch.token, pacing) {
        if let Err(undo) = unclaim(provider, &claim) {
            eprintln!("warning: could not fully undo the claim: {undo}");
        }
        return Err(e);
    }
    Ok(DispatchRecord {
        pool: id.pool.clone(),
        slot: claim.slot.name,
        machine: claim.slot.id,
        job: plan.job.clone(),
        model: plan.model.clone(),
        from: plan.refs.from.clone(),
        branch: plan.refs.branch.clone(),
        start: launch.start.to_string(),
        budget: plan.budget,
        at: now,
        read_only: plan.read_only,
        hardware: claim.hardware,
        abandoned: u32::try_from(claim.abandoned.len()).unwrap_or(u32::MAX),
        abandoned_cost_micros: claim.abandoned.iter().filter_map(|a| a.cost_micros).sum(),
    })
}

fn take_slot(
    provider: &dyn PoolProvider,
    id: &PoolId,
    launch: &Launch,
    now: u64,
) -> Result<Claim, VivariumError> {
    let plan = launch.plan;
    match provider.slot_lifetime() {
        SlotLifetime::Resting => claim_slot(provider, id, &plan.task, now),
        SlotLifetime::PerDispatch => {
            let seed = (launch.token, launch.start);
            open_slot(
                provider,
                &OpenRequest {
                    pool: id,
                    task: &plan.task,
                    now,
                    budget_secs: plan.budget,
                    spec: dispatch_spec(launch.config, id, seed, provider),
                    progress: launch.progress,
                    ready_timeout_secs: launch.ready_timeout,
                },
            )
        }
    }
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
    provider.put_file(id, &claim.hold, JOB_ARCHIVE, &archive)?;
    provider.release(id, &claim.hold)?;
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
