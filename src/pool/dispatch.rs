//! `dispatch`: plan a job, claim a slot, deliver the job as one upload, start
//! the slot. The caller records the dispatch on the task afterwards.

use std::time::Instant;

use sha2::{Digest, Sha256};

use super::bridge::bridge_brief;
use super::claim::{Claim, claim_slot, open_slot, unclaim};
use super::collect::Pacing;
use super::config::{PoolCommitter, PoolConfig};
use super::github::Secret;
use super::job::{self, JOB_ARCHIVE, JobPlan, build_bundle};
use super::progress::Progress;
use super::provider::{OpenRequest, PoolProvider, SlotLifetime};
use super::record::DispatchRecord;
use super::run::dispatch_spec;
use super::slot::PoolId;
use crate::error::VivariumError;

#[path = "target.rs"]
pub mod target;

/// Budget when `--budget` is not given.
pub const DEFAULT_BUDGET_SECS: u64 = 1800;
/// Bounds for `--budget`.
pub const BUDGET_RANGE: std::ops::RangeInclusive<u64> = 1..=43_200;

struct ResolvedJob {
    refs: job::Refs,
    with: Vec<job::WithRef>,
    budget: u64,
    reasoning: Option<String>,
    committer: PoolCommitter,
}

/// The flags of `vivi pool dispatch`.
#[derive(Debug, Clone, Default)]
pub struct DispatchFlags {
    pub model: String,
    /// Need provenance for a monitor row and synced dispatch receipt.
    pub need: Option<String>,
    /// Work/role kind for a monitor row and synced dispatch receipt.
    pub kind: Option<String>,
    pub from: Option<String>,
    pub branch: Option<String>,
    /// Remote branch ref the privileged runner publishes after agent success.
    pub publish_ref: Option<String>,
    /// Exact commit expected for guarded promotion publication.
    pub publish_expect: Option<String>,
    /// Extra refs the job may read (`--with`); when any are given they beat
    /// `with:` body lines, which [`job::resolve_with`] applies otherwise.
    pub with: Vec<String>,
    pub budget: Option<u64>,
    /// Allow pushing to the repository's default branch.
    pub allow_default_branch: bool,
    /// The job publishes nothing: detached checkout, no commit, no push, a
    /// token that cannot push. Excludes `branch`.
    pub read_only: bool,
    /// The reasoning effort level for `{reasoning}` in the harness template.
    /// Chosen by band at dispatch time (unit B4b); `None` refuses a template
    /// that needs one.
    pub reasoning: Option<String>,
}

/// A fresh job id: twelve hex digits.
#[must_use]
pub fn new_job_id(task: &str, nanos: u128, pid: u32) -> String {
    let digest = Sha256::digest(format!("{task}\0{nanos}\0{pid}"));
    hex::encode(digest)[..12].to_string()
}

/// Validate everything that can be checked without touching a machine and
/// assemble the job. `rules` is the standing-rules text to deliver. With the
/// pool's bridge enabled the brief is a pointer at the task on the server
/// ([`bridge_brief`]); without it, the task body itself.
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
    let (_, _, body) = task;
    validate_source(config, pool)?;
    let harness = config.harness.as_ref().ok_or_else(|| {
        VivariumError::Config(format!("[pools.{pool}.harness] is required to dispatch"))
    })?;
    let committer = config.committer.clone().ok_or_else(|| {
        VivariumError::Config(format!(
            "[pools.{pool}.committer] (name, email) is required to dispatch"
        ))
    })?;
    let reasoning = plan_reasoning(pool, &harness.run, flags)?;
    job::check_model(&flags.model, &harness.models)?;
    validate_publish_flags(flags)?;
    let refs = plan_refs(flags, body)?;
    let with = job::resolve_with(&flags.with, body, &refs.from, &refs.branch)?
        .into_iter()
        .map(|name| job::WithRef {
            name,
            commit: String::new(),
        })
        .collect();
    let budget = flags.budget.unwrap_or(DEFAULT_BUDGET_SECS);
    if !BUDGET_RANGE.contains(&budget) {
        return Err(VivariumError::Message(format!(
            "budget must be between {} and {} seconds",
            BUDGET_RANGE.start(),
            BUDGET_RANGE.end()
        )));
    }
    Ok(assemble_job_plan(
        config,
        task,
        flags,
        rules,
        job,
        harness,
        ResolvedJob {
            refs,
            with,
            budget,
            reasoning,
            committer,
        },
    ))
}

fn assemble_job_plan(
    config: &PoolConfig,
    task: (&str, &str, &str),
    flags: &DispatchFlags,
    rules: String,
    job: String,
    harness: &super::config::PoolHarness,
    resolved: ResolvedJob,
) -> JobPlan {
    let (handle, subject, body) = task;
    let ResolvedJob {
        refs,
        with,
        budget,
        reasoning,
        committer,
    } = resolved;
    JobPlan {
        target: None,
        prepared_cache_key: None,
        prepared_local_source: None,
        ramdisk_size_gib: None,
        ramdisk_env: Vec::new(),
        job,
        remote: config.remote_address(),
        checkout: None,
        task: handle.to_string(),
        need: flags.need.clone(),
        kind: flags.kind.clone(),
        subject: subject.to_string(),
        brief: brief_for(config, handle, subject, body),
        rules,
        refs,
        publish_ref: flags.publish_ref.clone(),
        publish_expect: flags.publish_expect.clone(),
        publish_remote: None,
        with,
        model: flags.model.clone(),
        budget,
        depth: config.fetch_depth(),
        harness_cmd: job::render_harness(&harness.run, &flags.model, reasoning.as_deref()),
        env_names: harness.env.clone(),
        committer,
        read_only: flags.read_only,
        facts: config.facts.clone(),
        restore: config.restore.clone(),
        bridge: None,
    }
}

fn validate_publish_flags(flags: &DispatchFlags) -> Result<(), VivariumError> {
    if flags.read_only && flags.publish_ref.is_some() {
        return Err(VivariumError::Config(
            "read-only jobs cannot request privileged publication".into(),
        ));
    }
    if flags.publish_ref.is_none() && flags.publish_expect.is_some() {
        return Err(VivariumError::Config(
            "publish expectation requires --publish-ref".into(),
        ));
    }
    if let Some(reference) = flags.publish_ref.as_deref() {
        let branch = reference.strip_prefix("refs/heads/").unwrap_or_default();
        if !valid_publish_ref(branch) {
            return Err(VivariumError::Config(format!(
                "publish ref must be a valid refs/heads branch, got {reference:?}"
            )));
        }
    }
    if let Some(expected) = flags.publish_expect.as_deref()
        && (!matches!(expected.len(), 40 | 64)
            || !expected
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    {
        return Err(VivariumError::Config(
            "publish expectation must be a full 40- or 64-character commit SHA".into(),
        ));
    }
    Ok(())
}

fn valid_publish_ref(branch: &str) -> bool {
    job::plain_ref(branch)
        && !branch.ends_with('.')
        && !branch.contains("@{")
        && branch
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

fn validate_source(config: &PoolConfig, pool: &str) -> Result<(), VivariumError> {
    if config.embedded.is_none() && config.worktree_source().is_none() {
        config.require_repo(pool, "dispatch")?;
    }
    Ok(())
}

/// The reasoning level a job runs at, when the harness template asks for
/// one: `flags.reasoning`, checked before anything else in `plan_job`
/// returns a plan so the refusal costs nothing. The value lands in a shell
/// command, so it is held to a safe charset.
fn plan_reasoning(
    pool: &str,
    run: &str,
    flags: &DispatchFlags,
) -> Result<Option<String>, VivariumError> {
    if !job::uses_reasoning(run) {
        return Ok(None);
    }
    let Some(level) = flags.reasoning.as_deref() else {
        return Err(VivariumError::Message(format!(
            "harness.run uses {{reasoning}} but no reasoning level was chosen for this job; \
             dispatch by band (a `band:` line in the task, or a role with a band) or remove \
             {{reasoning}} from [pools.{pool}.harness].run"
        )));
    };
    let safe = (1..=32).contains(&level.chars().count())
        && level
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !safe {
        return Err(VivariumError::Message(format!(
            "reasoning \"{level}\" is not allowed: use 1 to 32 characters from A-Z a-z 0-9 . _ -"
        )));
    }
    Ok(Some(level.to_string()))
}

/// The brief the worker reads: with the bridge on, a pointer at the task on
/// the server (the worker reads it with the vivi tools); without it, the
/// task body itself.
fn brief_for(config: &PoolConfig, handle: &str, subject: &str, body: &str) -> String {
    if config.bridge.enabled {
        bridge_brief(handle, subject)
    } else {
        body.to_string()
    }
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
    /// Origin branch to reset the claimed slot onto before the job starts.
    pub reset_base: Option<String>,
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
    if let Err(e) = deliver(provider, &claim, launch, pacing) {
        if let Err(undo) = unclaim(provider, &claim) {
            eprintln!("warning: could not fully undo the claim: {undo}");
        }
        return Err(e);
    }
    Ok(DispatchRecord {
        target: plan.target.as_ref().map(|t| t.id.clone()),
        repository: plan.target.as_ref().map(|t| t.source.clone()),
        pool: id.pool.clone(),
        need: plan.need.clone(),
        kind: plan.kind.clone(),
        slot: claim.slot.name,
        machine: claim.slot.id,
        job: plan.job.clone(),
        model: plan.model.clone(),
        from: plan.refs.from.clone(),
        branch: plan.refs.branch.clone(),
        start: launch.start.to_string(),
        with: plan
            .with
            .iter()
            .map(|w| format!("{}@{}", w.name, w.commit.get(..12).unwrap_or(&w.commit)))
            .collect(),
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
            let mut machine_config = launch.config.clone();
            let empty_token = Secret::new(String::new());
            let seed = if plan.target.is_some() {
                machine_config.repo = None;
                machine_config.embedded = None;
                machine_config.local.worktree_of = None;
                machine_config.build_dir = None;
                (&empty_token, "")
            } else {
                (launch.token, launch.start)
            };
            open_slot(
                provider,
                &OpenRequest {
                    pool: id,
                    task: &plan.task,
                    now,
                    budget_secs: plan.budget,
                    spec: dispatch_spec(&machine_config, id, seed, provider),
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
    launch: &Launch,
    pacing: &Pacing,
) -> Result<(), VivariumError> {
    let id = &claim.slot.id;
    let plan = launch.plan;
    if let Some(base) = launch.reset_base.as_deref() {
        super::reset::reset_claimed_slot(
            launch.config,
            provider,
            id,
            &claim.hold,
            base,
            "dispatch",
        )?;
    }
    let start = plan
        .checkout
        .as_ref()
        .map_or(plan.refs.from.as_str(), |c| c.start.as_str());
    let prepared_workspace = provider.prepare_job(id, plan.target.as_ref(), start, &plan.job)?;
    let mut bundle_plan = plan.clone();
    bundle_plan.prepared_cache_key = prepared_workspace
        .as_ref()
        .map(|workspace| workspace.cache_key.clone());
    bundle_plan.prepared_local_source =
        prepared_workspace.and_then(|workspace| workspace.local_source);
    if let Some((size_gib, env)) = provider.ramdisk_job_settings() {
        bundle_plan.ramdisk_size_gib = Some(size_gib);
        bundle_plan.ramdisk_env = env;
    }
    let archive = job::pack(&build_bundle(&bundle_plan, &claim.slot.name, launch.token))?;
    provider.put_file(id, &claim.hold, JOB_ARCHIVE, &archive)?;
    wait_stopped(provider, id, pacing)?;
    provider.start_claimed(id, &claim.hold)?;
    if let Err(error) = provider.release(id, &claim.hold) {
        eprintln!("warning: started slot {id}, but could not release its claim: {error}");
    }
    Ok(())
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
