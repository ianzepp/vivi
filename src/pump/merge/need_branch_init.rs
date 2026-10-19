//! Durable creation of the isolated branch for one planned need.

use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobOutcome, JobStatus, PoolJob};
use crate::pump::need::{self, NeedBranchInitResult};
use crate::pump::state::State;

/// Apply one terminal branch-initialization receipt. Unverifiable publication
/// remains attached to the same durable job and is retried on later ticks.
pub(super) fn apply_branch_init(ctx: &Ctx, st: &mut State, job: &PoolJob) {
    let Some(identity) = init_identity(job) else {
        fail_init(
            ctx,
            st,
            job,
            "need-branch-init-invalid",
            "branch initialization job has inconsistent durable identity",
        );
        return;
    };
    let Some(outcome) = job.outcome.as_ref().filter(|outcome| {
        job.status == JobStatus::Finished
            && outcome.state == "finished"
            && outcome.agent_success() == Some(true)
    }) else {
        fail_init(
            ctx,
            st,
            job,
            "need-branch-init-failed",
            "branch initialization worker did not report a clean exit",
        );
        return;
    };
    let receipt = match NeedBranchInitResult::parse(&outcome.receipt) {
        Ok(receipt) => receipt,
        Err(error) => {
            fail_init(ctx, st, job, "need-branch-init-invalid", &error);
            return;
        }
    };
    if !receipt_matches(&receipt, &identity) {
        fail_init(
            ctx,
            st,
            job,
            "need-branch-init-invalid",
            "branch initialization receipt does not match its durable assignment",
        );
        return;
    }
    match receipt.status.as_str() {
        "collision" => apply_collision(ctx, st, job, &identity, &receipt),
        "ready" | "exists" => complete_init(ctx, st, job, &identity, &receipt, outcome),
        _ => fail_init(
            ctx,
            st,
            job,
            "need-branch-init-blocked",
            "merge worker could not initialize the need branch",
        ),
    }
}

/// Validate the worker's target, need, branch, base and observed SHA.
fn receipt_matches(
    receipt: &NeedBranchInitResult,
    identity: &(String, String, u64, String, String, String),
) -> bool {
    receipt.version == 1
        && receipt.target == identity.0
        && receipt.need == identity.1
        && receipt.branch == identity.3
        && receipt.base_sha == identity.4
        && need::is_full_sha(&receipt.tip_sha)
}

/// Accept a collision only when the exact remote ref proves the claimed tip.
fn apply_collision(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    identity: &(String, String, u64, String, String, String),
    receipt: &NeedBranchInitResult,
) {
    let (target, need_id, _, branch, base_sha, refname) = identity;
    let Some(observed) = super::need_dispatch::observed_ref_sha(ctx, target, refname) else {
        ctx.log(&format!(
            "need branch collision verification pending {target}/{need_id}"
        ));
        return;
    };
    if observed == base_sha.as_str() || observed != receipt.tip_sha {
        ctx.log(&format!(
            "need branch collision evidence mismatch {target}/{need_id}"
        ));
        return;
    }
    let detail = format!("need branch {branch} exists at an unrelated tip {observed}");
    if need::record_collision(st, target, need_id, &job.id, &detail).is_ok()
        && jobs::mark_applied(st, &job.id, ctx.now())
    {
        ctx.exception("need-branch-collision", need_id, &detail);
    }
}

/// Record the branch only after matching runner and independent remote receipts.
fn complete_init(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    identity: &(String, String, u64, String, String, String),
    receipt: &NeedBranchInitResult,
    outcome: &JobOutcome,
) {
    let (target, need_id, revision, _, base_sha, refname) = identity;
    let Some(observed) = super::need_dispatch::observed_ref_sha(ctx, target, refname) else {
        ctx.log(&format!(
            "need branch publication verification pending {target}/{need_id}"
        ));
        return;
    };
    let Some(publication) = outcome.runner_publish() else {
        ctx.log(&format!(
            "need branch runner receipt pending {target}/{need_id}"
        ));
        return;
    };
    if !publication_matches(&publication, receipt, base_sha, refname, &observed) {
        handle_mismatch(ctx, st, job, target, need_id, base_sha, refname, &observed);
        return;
    }
    if need::record_created(st, target, need_id, &job.id, base_sha, &observed).is_err() {
        ctx.log(&format!(
            "need branch creation receipt rejected {target}/{need_id} revision={revision}"
        ));
        return;
    }
    if jobs::mark_applied(st, &job.id, ctx.now()) {
        ctx.log(&format!(
            "need branch active {target}/{need_id} at {}",
            short(&observed)
        ));
        if let Err(error) = super::need_dispatch::refresh_freezes(ctx, st) {
            ctx.log(&format!("need freeze refresh failed: {error}"));
        }
    }
}

/// Bind a successful runner receipt to the exact branch base.
fn publication_matches(
    publication: &jobs::RunnerPublishReceipt,
    receipt: &NeedBranchInitResult,
    base_sha: &str,
    refname: &str,
    observed: &str,
) -> bool {
    publication.succeeded()
        && publication.reference == refname
        && publication.candidate == base_sha
        && publication.remote_sha == observed
        && observed == base_sha
        && receipt.tip_sha == base_sha
}

/// Park an unrelated existing ref as a collision; otherwise retry verification.
fn handle_mismatch(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    target: &str,
    need_id: &str,
    base_sha: &str,
    refname: &str,
    observed: &str,
) {
    if observed != base_sha {
        let detail =
            format!("need branch ref {refname} is at {observed}, expected base {base_sha}");
        if need::record_collision(st, target, need_id, &job.id, &detail).is_ok()
            && jobs::mark_applied(st, &job.id, ctx.now())
        {
            ctx.exception("need-branch-collision", need_id, &detail);
        }
    } else {
        ctx.log(&format!(
            "need branch runner verification pending {target}/{need_id}"
        ));
    }
}

/// Extract and validate the durable branch-init job identity.
fn init_identity(job: &PoolJob) -> Option<(String, String, u64, String, String, String)> {
    if job.kind != "need-branch-init" {
        return None;
    }
    let target = string_field(&job.payload, "target")?;
    let need_id = string_field(&job.payload, "need")?;
    let revision = job.payload.get("revision")?.as_u64()?;
    let branch = string_field(&job.payload, "branch")?;
    let base_sha = string_field(&job.payload, "base_sha")?;
    let refname = string_field(&job.payload, "ref")?;
    (job.target.as_deref() == Some(target.as_str())
        && revision > 0
        && !need_id.is_empty()
        && job.from_ref.as_deref() == Some(base_sha.as_str())
        && job.publish_ref.as_deref() == Some(refname.as_str())
        && job.publish_expect.as_deref() == Some(base_sha.as_str())
        && refname == format!("refs/heads/{branch}")
        && branch == need::need_branch(&need_id)
        && need::is_full_sha(&base_sha)
        && job.units.is_empty()
        && job.source_jobs.is_empty())
    .then_some((target, need_id, revision, branch, base_sha, refname))
}

/// Record a retryable initialization failure and settle the current outbox job.
fn fail_init(ctx: &Ctx, st: &mut State, job: &PoolJob, why: &str, detail: &str) {
    if let Some((target, need_id, _, _, _, _)) = init_identity(job) {
        if need::record_creation_failure(st, &target, &need_id, &job.id, detail).is_err() {
            ctx.log(&format!(
                "need branch failure could not be recorded {target}/{need_id}"
            ));
            return;
        }
    }
    if jobs::mark_applied(st, &job.id, ctx.now()) {
        ctx.exception(why, &job.id, detail);
    }
}

fn string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value.get(field)?.as_str().map(str::to_owned)
}

fn short(value: &str) -> String {
    value.chars().take(10).collect()
}
