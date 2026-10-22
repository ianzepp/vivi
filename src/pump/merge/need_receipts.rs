//! Receipt validation and application for need-owned unit merge jobs.

use std::collections::{BTreeMap, BTreeSet};

use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobStatus, PoolJob};
use crate::pump::need::{self};
use crate::pump::state::{MergeResult, State, Status};

use super::apply::{apply_failure, apply_merged_need, apply_undecided};
use super::pool_dispatch;

struct NeedMergeIdentity {
    target: String,
    need: String,
    revision: u64,
    branch: String,
    base_sha: String,
    starting_tip: String,
    refname: String,
}

/// Apply one durable unit merge for a need branch and bind it to its captured
/// revision, base and target ref.
pub(super) fn apply_need_merge(ctx: &Ctx, st: &mut State, job: &PoolJob) {
    let Some(identity) = merge_identity(job) else {
        reject_need_merge(
            ctx,
            st,
            job,
            "need-merge-invalid",
            "need merge job has inconsistent durable identity",
        );
        return;
    };
    let Some(outcome) = job.outcome.as_ref().filter(|outcome| {
        job.status == JobStatus::Finished
            && outcome.state == "finished"
            && outcome.agent_success() == Some(true)
    }) else {
        settle_unverified_need_merge(
            ctx,
            st,
            job,
            &identity,
            "worker did not report a clean exit",
        );
        return;
    };
    let receipt = match checked_need_receipt(st, job, outcome, &identity) {
        Ok(receipt) => receipt,
        Err(reason) => {
            settle_need_merge(
                ctx,
                st,
                job,
                &identity,
                &format!("receipt did not match its unit assignment: {reason}"),
                true,
            );
            return;
        }
    };
    let Some(tip) = verified_need_tip(ctx, st, job, outcome, &receipt, &identity) else {
        return;
    };
    let Some(current) = record_need_tip(st, &identity, &tip) else {
        ctx.log(&format!(
            "need merge lifecycle or tip rejected {}/{} job={}",
            identity.target, identity.need, job.id
        ));
        return;
    };
    finish_need_merge(ctx, st, job, &identity, &receipt, &tip, current);
}

/// Parse the worker receipt and check it against the durable assignment,
/// naming the first mismatch. A `job` entry naming this very job is not a
/// requested source and is dropped before checking.
fn checked_need_receipt(
    st: &State,
    job: &PoolJob,
    outcome: &jobs::JobOutcome,
    identity: &NeedMergeIdentity,
) -> Result<pool_dispatch::MergeReceipt, String> {
    let mut receipt = pool_dispatch::parse_receipt(&outcome.receipt).ok_or_else(|| {
        String::from("no PUMP_MERGE_RESULT line holding a parsable merge receipt")
    })?;
    receipt
        .sources
        .retain(|source| !(source.kind == "job" && source.id == job.id));
    let units: BTreeSet<String> = job.units.iter().cloned().collect();
    let assigned_sources = assigned_unit_sources(job, st, identity).ok_or_else(|| {
        String::from("the durable unit_sources no longer match the job or its units")
    })?;
    if let Some(reason) =
        pool_dispatch::result_failure(&receipt, job, outcome, &units, &BTreeSet::new())
    {
        return Err(reason);
    }
    if let Some(source) = receipt.sources.iter().find(|source| {
        !source_belongs_to_need(
            st,
            source,
            &identity.need,
            &identity.target,
            &assigned_sources,
        )
    }) {
        return Err(format!(
            "{} source `{}` does not belong to this need, target and assignment",
            source.kind, source.id
        ));
    }
    if let Some(id) = unproven_merged_source(outcome, &receipt, &assigned_sources) {
        return Err(format!(
            "merged unit `{id}` has no single runner ancestry proof of its assigned SHA"
        ));
    }
    Ok(receipt)
}

/// Capture the unit branch and SHA assigned when this durable job was queued.
fn assigned_unit_sources(
    job: &PoolJob,
    st: &State,
    identity: &NeedMergeIdentity,
) -> Option<BTreeMap<String, (String, String)>> {
    let rows = job.payload.get("unit_sources")?.as_array()?;
    if rows.len() != job.units.len() || job.with.len() != job.units.len() {
        return None;
    }
    let mut sources = BTreeMap::new();
    for row in rows {
        let id = row.get("id")?.as_str()?;
        let branch = row.get("branch")?.as_str()?;
        let source_ref = row.get("source_ref")?.as_str()?;
        let source_sha = row.get("source_sha")?.as_str()?;
        let revision = row.get("plan_revision")?.as_u64()?;
        let unit = st.units.get(id)?;
        let assigned_ref = if branch.starts_with("refs/heads/") {
            branch.to_owned()
        } else {
            format!("refs/heads/{branch}")
        };
        if !job.units.iter().any(|uid| uid == id)
            || row.get("need")?.as_str()? != identity.need
            || row.get("target")?.as_str()? != identity.target
            || revision != identity.revision
            || unit.need != identity.need
            || unit.target_id() != identity.target
            || unit.branch.as_deref() != Some(branch)
            || source_ref != assigned_ref
            || !pool_dispatch::valid_sha(source_sha)
            || !job.with.iter().any(|with| with == branch)
            || sources
                .insert(
                    id.to_owned(),
                    (source_ref.to_owned(), source_sha.to_owned()),
                )
                .is_some()
        {
            return None;
        }
    }
    (sources.len() == job.units.len()).then_some(sources)
}

/// Require an exact private-runner ancestry proof for every merged unit;
/// returns the first merged unit without one.
fn unproven_merged_source(
    outcome: &jobs::JobOutcome,
    receipt: &pool_dispatch::MergeReceipt,
    assigned: &BTreeMap<String, (String, String)>,
) -> Option<String> {
    let proofs = outcome.runner_sources();
    receipt
        .sources
        .iter()
        .filter(|source| source.status == "merged")
        .find(|source| {
            let Some((reference, sha)) = assigned.get(&source.id) else {
                return true;
            };
            let matching: Vec<_> = proofs
                .iter()
                .filter(|proof| proof.reference == *reference)
                .collect();
            !(matching.len() == 1 && matching[0].sha == *sha && matching[0].contained)
        })
        .map(|source| source.id.clone())
}

fn verified_need_tip(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    outcome: &jobs::JobOutcome,
    receipt: &pool_dispatch::MergeReceipt,
    identity: &NeedMergeIdentity,
) -> Option<String> {
    let Some(observed) =
        super::need_dispatch::observed_ref_sha(ctx, &identity.target, &identity.refname)
    else {
        ctx.log(&format!(
            "need merge publication verification pending {}/{} job={}",
            identity.target, identity.need, job.id
        ));
        return None;
    };
    let Some(publication) = outcome.runner_publish() else {
        settle_unverified_need_merge(
            ctx,
            st,
            job,
            identity,
            "runner publication receipt is missing",
        );
        return None;
    };
    if publication.succeeded()
        && publication.reference == identity.refname
        && publication.candidate == receipt.tip
        && publication.remote_sha == observed
        && observed == receipt.tip
        && job.publish_contains.as_deref() == Some(identity.starting_tip.as_str())
        && publication.contains.as_deref() == Some(identity.starting_tip.as_str())
        && publication.contains_verified
    {
        return Some(observed);
    }
    settle_unverified_need_merge(
        ctx,
        st,
        job,
        identity,
        "runner publication did not match the observed need ref",
    );
    None
}

fn settle_unverified_need_merge(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    identity: &NeedMergeIdentity,
    detail: &str,
) {
    settle_need_merge(ctx, st, job, identity, detail, false);
}

/// Settle a need merge whose result cannot be accepted. `invalid_receipt`
/// counts it toward the consecutive-invalid-receipt hold.
fn settle_need_merge(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    identity: &NeedMergeIdentity,
    detail: &str,
    invalid_receipt: bool,
) {
    let Some(observed) =
        super::need_dispatch::observed_ref_sha(ctx, &identity.target, &identity.refname)
    else {
        ctx.log(&format!(
            "need merge outcome remains pending remote ref verification job={}",
            job.id
        ));
        return;
    };
    if observed != identity.starting_tip {
        let detail = format!("job {}: {detail}; observed tip {observed}", job.id);
        if need::record_unverified_tip(
            st,
            &identity.target,
            &identity.need,
            &job.id,
            &observed,
            &detail,
        )
        .is_err()
        {
            ctx.log(&format!(
                "need merge unverified tip could not be recorded {}/{}",
                identity.target, identity.need
            ));
            return;
        }
    }
    reset_need_units(st, job);
    if !jobs::mark_applied(st, &job.id, ctx.now()) {
        return;
    }
    let streak = invalid_receipt
        .then(|| {
            need::record_invalid_receipt(
                st,
                &identity.target,
                &identity.need,
                identity.revision,
                &format!("{}: {detail}", job.id),
            )
            .ok()
        })
        .flatten();
    if observed == identity.starting_tip {
        if let Some(entries) = streak.filter(|entries| entries.len() == need::INVALID_RECEIPT_LIMIT)
        {
            let ids: Vec<&str> = entries
                .iter()
                .map(|entry| entry.split(": ").next().unwrap_or(entry))
                .collect();
            ctx.exception(
                "need-merge-receipt-invalid",
                &identity.need,
                &format!(
                    "{} consecutive invalid merge receipts (jobs {}); unit merges for revision {} are held until the plan is revised. First failure, job {}",
                    entries.len(),
                    ids.join(", "),
                    identity.revision,
                    entries[0]
                ),
            );
            return;
        }
        ctx.exception(
            "need-merge-unverified",
            &identity.need,
            &format!("job {}: {detail}; observed tip {}", job.id, observed),
        );
    } else {
        ctx.exception(
            "need-branch-unverified-tip",
            &identity.need,
            &format!("job {}: {detail}; observed tip {}", job.id, observed),
        );
    }
}

fn reset_need_units(st: &mut State, job: &PoolJob) {
    for uid in &job.units {
        if let Some(unit) = st.units.get_mut(uid)
            && unit.status == Status::Merging
        {
            unit.status = Status::Delivered;
        }
    }
}

fn record_need_tip(st: &mut State, identity: &NeedMergeIdentity, tip: &str) -> Option<bool> {
    let record = match need::get(st, &identity.target, &identity.need) {
        Ok(Some(record)) => record,
        Ok(None) | Err(_) => return None,
    };
    if record.branch != identity.branch || record.base_sha != identity.base_sha {
        return None;
    }
    let current = st
        .needs
        .get(&identity.need)
        .is_some_and(|plan| plan.revision == identity.revision);
    let result = if current {
        need::record_tip(
            st,
            &identity.target,
            &identity.need,
            identity.revision,
            &identity.base_sha,
            tip,
        )
    } else {
        need::observe_branch_tip(st, &identity.target, &identity.need, tip)
    };
    result.is_ok().then_some(current)
}

fn finish_need_merge(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    identity: &NeedMergeIdentity,
    receipt: &pool_dispatch::MergeReceipt,
    tip: &str,
    current: bool,
) {
    for source in &receipt.sources {
        apply_need_source(
            ctx,
            st,
            job,
            source,
            tip,
            identity.revision,
            current,
            &identity.need,
        );
    }
    if !jobs::mark_applied(st, &job.id, ctx.now()) {
        return;
    }
    if need::clear_invalid_receipts(st, &identity.target, &identity.need).is_err() {
        ctx.log(&format!(
            "need invalid-receipt streak could not be cleared {}/{}",
            identity.target, identity.need
        ));
    }
    if !current {
        ctx.exception(
            "need-merge-stale-revision",
            &identity.need,
            &format!(
                "applied published branch tip from revision {}; current revision has changed",
                identity.revision
            ),
        );
    }
    if let Err(error) = super::need_dispatch::refresh_freezes(ctx, st) {
        ctx.log(&format!("need freeze refresh failed: {error}"));
    }
}

/// Extract and validate a need merge job's durable target and range.
fn merge_identity(job: &PoolJob) -> Option<NeedMergeIdentity> {
    if !super::need_dispatch::is_need_merge_job(job) {
        return None;
    }
    let target = super::need_dispatch::string_field(&job.payload, "target")?;
    let need_id = super::need_dispatch::string_field(&job.payload, "need")?;
    let revision = job.payload.get("plan_revision")?.as_u64()?;
    let branch = super::need_dispatch::string_field(&job.payload, "need_branch")?;
    let need_ref = super::need_dispatch::string_field(&job.payload, "need_ref")?;
    let base_sha = super::need_dispatch::string_field(&job.payload, "frozen_base_sha")?;
    let starting_tip = super::need_dispatch::string_field(&job.payload, "starting_tip_sha")?;
    (job.target.as_deref() == Some(target.as_str())
        && revision > 0
        && job.from_ref.as_deref() == Some(starting_tip.as_str())
        && job.publish_ref.as_deref() == Some(need_ref.as_str())
        && job.publish_expect.is_none()
        && branch == need::need_branch(&need_id)
        && need_ref == format!("refs/heads/{branch}")
        && need::is_full_sha(&base_sha)
        && need::is_full_sha(&starting_tip)
        && !job.units.is_empty()
        && job.source_jobs.is_empty())
    .then_some(NeedMergeIdentity {
        target,
        need: need_id,
        revision,
        branch,
        base_sha,
        starting_tip,
        refname: need_ref,
    })
}

/// Whether one source still names the captured need, target and assignment.
fn source_belongs_to_need(
    st: &State,
    source: &pool_dispatch::SourceResult,
    need_id: &str,
    target: &str,
    assigned: &BTreeMap<String, (String, String)>,
) -> bool {
    source.kind == "unit"
        && assigned.contains_key(&source.id)
        && st
            .units
            .get(&source.id)
            .is_some_and(|unit| unit.need == need_id && unit.target_id() == target)
}

/// Apply one source only while its unit still belongs to the captured plan revision.
fn apply_need_source(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    source: &pool_dispatch::SourceResult,
    tip: &str,
    revision: u64,
    current: bool,
    need_id: &str,
) {
    let Some(unit) = st.units.get(&source.id) else {
        return;
    };
    if !current
        || unit.revision != revision
        || unit.status != Status::Merging
        || !job.units.contains(&source.id)
    {
        if unit.status == Status::Merging
            && let Some(unit) = st.units.get_mut(&source.id)
        {
            unit.status = Status::Delivered;
        }
        return;
    }
    let result = MergeResult {
        uid: source.id.clone(),
        ok: Some(source.status == "merged"),
        tip: Some(tip.to_owned()),
        outside: source.outside.clone(),
        secs: source.secs,
        why: (!source.why.is_empty()).then(|| source.why.clone()),
        detail: (!source.detail.is_empty()).then(|| source.detail.clone()),
        ..MergeResult::default()
    };
    match source.status.as_str() {
        "merged" => apply_merged_need(ctx, st, &result),
        "dropped" => apply_failure(ctx, st, &result),
        "undecided" => {
            super::pool_dispatch::mark_solo_retry(st, &source.id, &source.why);
            apply_undecided(ctx, st, &result);
        }
        _ => ctx.log(&format!(
            "need merge result ignored {}:{}",
            need_id, source.id
        )),
    }
}

/// Reject an invalid need merge while retaining unit work for a safe retry.
fn reject_need_merge(ctx: &Ctx, st: &mut State, job: &PoolJob, why: &str, detail: &str) {
    if !record_unverified_job_tip(ctx, st, job, detail) {
        ctx.log(&format!(
            "invalid need merge remains pending branch observation job={}",
            job.id
        ));
        return;
    }
    reset_need_units(st, job);
    if jobs::mark_applied(st, &job.id, ctx.now()) {
        ctx.exception(why, &job.id, detail);
    }
}

/// Preserve an unexpected remote tip separately before settling a malformed job.
fn record_unverified_job_tip(ctx: &Ctx, st: &mut State, job: &PoolJob, detail: &str) -> bool {
    let Some(target) = job.target.as_deref() else {
        return true;
    };
    let Some(need_id) = super::need_dispatch::string_field(&job.payload, "need") else {
        return true;
    };
    let expected_ref = format!("refs/heads/{}", need::need_branch(&need_id));
    if job.publish_ref.as_deref() != Some(expected_ref.as_str()) {
        return true;
    }
    let Some(observed) = super::need_dispatch::observed_ref_sha(ctx, target, &expected_ref) else {
        return false;
    };
    let Ok(Some(branch)) = need::get(st, target, &need_id) else {
        return false;
    };
    if branch.branch_tip_sha.as_deref() == Some(observed.as_str()) {
        return true;
    }
    need::record_unverified_tip(st, target, &need_id, &job.id, &observed, detail).is_ok()
}
