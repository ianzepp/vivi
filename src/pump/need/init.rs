//! Durable branch-init job construction and first-dispatch coordination.

use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{self, JobSpec};
use crate::pump::state::State;

use super::{NeedBranch, NeedBranchStatus, binding_for, get, record_create_job, reserve};

const INIT_RESULT_PREFIX: &str = "PUMP_NEED_BRANCH_RESULT=";

struct InitJobFields {
    id: String,
    branch: String,
    refname: String,
    payload: serde_json::Value,
}

/// One branch initialization worker's structured result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NeedBranchInitResult {
    /// Receipt format version.
    pub version: u32,
    /// Repository target.
    pub target: String,
    /// Need handle.
    pub need: String,
    /// Exact need branch ref.
    pub branch: String,
    /// Worker outcome: `ready`, `exists`, `collision`, or `blocked`.
    pub status: String,
    /// Reserved initial commit.
    pub base_sha: String,
    /// Ref tip observed by the worker.
    pub tip_sha: String,
}

impl NeedBranchInitResult {
    /// Parse the worker's structured result line.
    #[must_use]
    pub fn parse(receipt: &str) -> Result<Self, String> {
        let line = receipt
            .lines()
            .find_map(|line| line.trim().strip_prefix(INIT_RESULT_PREFIX))
            .ok_or_else(|| String::from("branch initialization result marker is missing"))?;
        serde_json::from_str(line)
            .map_err(|error| format!("invalid branch initialization result: {error}"))
    }
}

/// Reserve or recover branch initialization before the first unit dispatch.
///
/// Only a read-only `ls-remote` runs here. Branch creation is performed by a
/// durable Merge-role pool job; unit work remains pending until its receipt is
/// independently verified and applied.
pub fn ensure_creation(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
) -> Result<bool, VivariumError> {
    if !st.needs.contains_key(need) && get(st, target, need)?.is_none() {
        return Ok(true);
    }
    if st
        .needs
        .get(need)
        .is_some_and(|record| record.amendment_pending)
    {
        return Ok(false);
    }
    if let Some(record) = get(st, target, need)? {
        if record.landing_job_id.is_some() && record.status != NeedBranchStatus::Landed {
            return Ok(false);
        }
        if record.status == NeedBranchStatus::Unverified {
            return Ok(false);
        }
        if record.status == NeedBranchStatus::Landed {
            return recreate_landed(ctx, st, target, need, revision, &record);
        }
        if revision > record.revision {
            super::advance_revision(st, target, need, revision)?;
        }
        let latest = super::require(st, target, need)?;
        if latest.status == NeedBranchStatus::Active && latest.revision == revision {
            return Ok(true);
        }
        if matches!(
            latest.status,
            NeedBranchStatus::Collision | NeedBranchStatus::Unverified
        ) {
            return Ok(false);
        }
        if latest.status == NeedBranchStatus::Frozen {
            if binding_for(st, target, need)?.is_none() {
                super::reopen(st, target, need)?;
                return Ok(true);
            }
            return Ok(false);
        }
    } else {
        let base = observed_integration_tip(ctx, target)?;
        reserve(st, target, need, revision, &base)?;
    }
    enqueue_init(ctx, st, target, need, revision)?;
    Ok(false)
}

fn recreate_landed(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
    record: &NeedBranch,
) -> Result<bool, VivariumError> {
    if revision <= record.revision
        || record
            .cleanup_receipt
            .as_ref()
            .is_none_or(|receipt| !receipt.verified_absent)
    {
        return Ok(false);
    }
    let base = observed_integration_tip(ctx, target)?;
    reserve(st, target, need, revision, &base)?;
    enqueue_init(ctx, st, target, need, revision)?;
    Ok(false)
}

/// Reopen a stale freeze after same-revision membership or unit state changed.
pub fn reopen(st: &mut State, target: &str, need: &str) -> Result<(), VivariumError> {
    let mut record = super::require(st, target, need)?;
    if record.landing_job_id.is_some() || record.status != NeedBranchStatus::Frozen {
        return Err(super::state_error(
            "only an unlocked frozen branch can be reopened",
        ));
    }
    record.frozen_tip_sha = None;
    record.frozen_units.clear();
    record.unit_fingerprint = None;
    record.status = NeedBranchStatus::Active;
    super::write(st, record)?;
    Ok(())
}

/// Construct and enqueue the durable Merge-role branch-init job.
pub fn enqueue_init(
    ctx: &Ctx,
    st: &mut State,
    target: &str,
    need: &str,
    revision: u64,
) -> Result<(), VivariumError> {
    let mut record = super::require(st, target, need)?;
    if record.status != NeedBranchStatus::Creating {
        return Ok(());
    }
    if wait_for_or_retry_existing(st, &mut record)? {
        return Ok(());
    }
    let Some(spec) = init_job_spec(ctx, target, need, revision, &record)? else {
        return Ok(());
    };
    let id = spec.id.clone();
    jobs::enqueue(st, spec)?;
    record_create_job(st, target, need, &id)
}

fn wait_for_or_retry_existing(
    st: &mut State,
    record: &mut super::NeedBranch,
) -> Result<bool, VivariumError> {
    let Some(id) = record.create_job_id.as_deref() else {
        return Ok(false);
    };
    let Some(job) = st.jobs.get(id) else {
        return Ok(false);
    };
    if !job.status.is_terminal() || job.applied_at.is_none() {
        return Ok(true);
    }
    record.create_attempt = record.create_attempt.saturating_add(1).max(1);
    record.create_job_id = None;
    record.error.get_or_insert_with(|| {
        String::from("prior branch initialization ended without applying a receipt")
    });
    super::write(st, record.clone())?;
    Ok(false)
}

fn init_job_spec(
    ctx: &Ctx,
    target: &str,
    need: &str,
    revision: u64,
    record: &super::NeedBranch,
) -> Result<Option<JobSpec>, VivariumError> {
    let Some((target_ctx, role)) = init_target_role(ctx, target, need)? else {
        return Ok(None);
    };
    let id = init_job_id(
        &record.target,
        &record.need,
        record.revision,
        &record.base_sha,
        record.create_attempt,
    );
    let branch = record.branch.clone();
    let remote = target_ctx.cfg.remote.clone();
    let refname = format!("refs/heads/{branch}");
    let payload = json!({
        "target": target,
        "need": need,
        "revision": revision,
        "branch": branch,
        "base_sha": record.base_sha,
        "remote": remote,
        "ref": refname,
    });
    Ok(Some(make_init_spec(
        target,
        &target_ctx,
        record,
        &role,
        InitJobFields {
            id,
            branch,
            refname,
            payload,
        },
    )))
}

fn init_target_role(
    ctx: &Ctx,
    target: &str,
    need: &str,
) -> Result<Option<(Ctx, crate::routing::RoutedRole)>, VivariumError> {
    let target_ctx = ctx
        .for_target(target)
        .ok_or_else(|| super::state_error(&format!("unknown repository target {target:?}")))?;
    let role = ctx.routing.merge().filter(|role| {
        role.status == "active"
            && role
                .pool
                .as_ref()
                .is_some_and(|pool| ctx.routing.pools.contains_key(pool))
    });
    let Some(role) = role else {
        ctx.exception(
            "need-branch-init-route-missing",
            need,
            "the need branch waits for an active Merge role assigned to a configured pool",
        );
        return Ok(None);
    };
    Ok(Some((target_ctx, role.clone())))
}

fn make_init_spec(
    target: &str,
    target_ctx: &Ctx,
    record: &super::NeedBranch,
    role: &crate::routing::RoutedRole,
    fields: InitJobFields,
) -> JobSpec {
    let mut spec = JobSpec::new(
        fields.id,
        "need-branch-init",
        &role.name,
        &format!("initialize need branch {}", fields.branch),
        init_brief(&fields.payload),
    );
    spec.builtin_role = Some(String::from("merge"));
    spec.pool_candidates = role.pool.iter().cloned().collect();
    spec.target = Some(target.to_owned());
    spec.from_ref = Some(record.base_sha.clone());
    spec.branch = Some(fields.branch);
    spec.publish_ref = Some(fields.refname);
    spec.publish_expect = Some(record.base_sha.clone());
    spec.budget = target_ctx
        .cfg
        .merge
        .remote_limit
        .max(target_ctx.cfg.dispatch.budget);
    spec.payload = fields.payload;
    spec
}

fn init_brief(payload: &serde_json::Value) -> String {
    format!(
        "Initialize this need's isolated branch. The checkout starts at the exact base commit on a local branch with the assigned name. Inspect the remote ref first. If absent, report ready; if it already equals the base SHA, report exists; if it is at another SHA, report collision and make no change. If local HEAD differs from base, report blocked. Do not reset, overwrite, delete, create a commit, or push. The privileged pool runner publishes only the assigned base SHA to the assigned ref after your clean exit.\n\nEnd with one minified JSON line prefixed `{INIT_RESULT_PREFIX}`: {{\"version\":1,\"status\":\"ready|exists|collision|blocked\",\"target\":\"...\",\"need\":\"...\",\"branch\":\"...\",\"base_sha\":\"...\",\"tip_sha\":\"...\"}}.\n\nPayload:\n{payload}"
    )
}

fn observed_integration_tip(ctx: &Ctx, target: &str) -> Result<String, VivariumError> {
    let target_ctx = ctx
        .for_target(target)
        .ok_or_else(|| super::state_error(&format!("unknown repository target {target:?}")))?;
    let reference = format!("refs/heads/{}", target_ctx.cfg.branch);
    let output = target_ctx.git(
        &format!(
            "ls-remote {} {}",
            shell_quote(&target_ctx.cfg.remote),
            shell_quote(&reference),
        ),
        30,
    );
    if output.code != 0 {
        return Err(super::state_error(&format!(
            "cannot observe target integration tip for {target}: {}",
            output.text.trim()
        )));
    }
    let expected_ref = format!("refs/heads/{}", target_ctx.cfg.branch);
    let mut rows = output.text.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        let sha = fields.next()?;
        let reference = fields.next()?;
        (reference == expected_ref).then_some(sha)
    });
    let sha = rows.next().ok_or_else(|| {
        super::state_error(&format!(
            "remote integration ref is absent for target {target}"
        ))
    })?;
    if rows.next().is_some() || !super::is_full_sha(sha) {
        return Err(super::state_error(&format!(
            "remote integration ref for {target} is ambiguous or malformed"
        )));
    }
    Ok(sha.to_owned())
}

fn init_job_id(target: &str, need: &str, revision: u64, base_sha: &str, attempt: u32) -> String {
    let digest =
        Sha256::digest(format!("{target}\0{need}\0{revision}\0{base_sha}\0{attempt}").as_bytes());
    format!(
        "need-init-{}-r{revision}-a{attempt}",
        hex::encode(&digest[..6])
    )
}
