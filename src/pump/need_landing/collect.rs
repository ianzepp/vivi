use std::collections::BTreeSet;

use crate::error::VivariumError;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{self, JobStatus, PoolJob};
use crate::pump::marked_result::{ResultForm, find_marked, note_unprefixed};
use crate::pump::need::{
    self, NeedAuditBinding, NeedBranch, NeedCleanupReceipt, NeedLandingReceipt,
};
use crate::pump::need_verdict::{self, AcceptedNeedReceipt};
use crate::pump::state::State;

use super::{
    MERGE_RESULT, NeedMergeCheck, NeedMergeResult, NeedMergeSource, RequiredCheck, full_sha,
    verdict_name,
};

/// Collect terminal landing and cleanup jobs. A missing remote read is retried
/// on the next tick; uncertain publication keeps its branch locked.
pub(crate) fn collect_completed(ctx: &Ctx, st: &mut State) -> Result<(), VivariumError> {
    let jobs: Vec<PoolJob> = st
        .jobs
        .values()
        .filter(|job| {
            matches!(job.kind.as_str(), "need-merge" | "need-cleanup")
                && job.status.is_terminal()
                && job.applied_at.is_none()
                && job.outcome.is_some()
        })
        .cloned()
        .collect();
    for job in jobs {
        match job.kind.as_str() {
            "need-merge" => collect_landing(ctx, st, &job)?,
            "need-cleanup" => collect_cleanup(ctx, st, &job)?,
            _ => {}
        }
    }
    settle_gated_landings(ctx, st);
    Ok(())
}
struct LandingProof {
    accepted: AcceptedNeedReceipt,
    binding: NeedAuditBinding,
    target_ref: String,
    result: NeedMergeResult,
    publication: crate::pump::jobs::RunnerPublishReceipt,
}

fn collect_landing(ctx: &Ctx, st: &mut State, job: &PoolJob) -> Result<(), VivariumError> {
    let (target, need_id, binding) = match landing_job_identity(job) {
        Ok(identity) => identity,
        Err(error) => {
            fail_landing(
                ctx,
                st,
                job,
                &format!("need-merge job payload has an invalid frozen binding: {error}"),
            )?;
            return Ok(());
        }
    };
    let Some(branch) = need::get(st, &target, &need_id)? else {
        fail_landing(
            ctx,
            st,
            job,
            "need branch record disappeared before landing collection",
        )?;
        return Ok(());
    };
    if branch.landing_receipt.is_some() {
        return Ok(());
    }
    if !job_owns_frozen_branch(ctx, job, &branch, &need_id) {
        return Ok(());
    }
    let Some(outcome) = job.outcome.as_ref() else {
        return Ok(());
    };
    if outcome.agent_success() == Some(false) {
        fail_landing(
            ctx,
            st,
            job,
            "Merge role exited unsuccessfully; no candidate was accepted",
        )?;
        return Ok(());
    }
    let Some(proof) = prepare_landing_proof(ctx, st, job, &target, &need_id, &binding, outcome)?
    else {
        return Ok(());
    };
    let Some(observed) = verify_published_target(ctx, job, &need_id, &proof) else {
        return Ok(());
    };
    record_verified_landing(ctx, st, job, proof, &observed)
}

fn job_owns_frozen_branch(ctx: &Ctx, job: &PoolJob, branch: &NeedBranch, need_id: &str) -> bool {
    if branch.landing_job_id.as_deref() != Some(job.id.as_str()) {
        hold_landing(
            ctx,
            job,
            need_id,
            "terminal job does not own the branch landing reservation",
        );
        return false;
    }
    let source = job
        .payload
        .get("source_branch")
        .and_then(serde_json::Value::as_str);
    if source != Some(branch.branch.as_str())
        || !matches!(job.with.as_slice(), [sha] if Some(sha) == branch.frozen_tip_sha.as_ref())
    {
        hold_landing(
            ctx,
            job,
            need_id,
            "landing job does not bind the owned need branch and exact frozen source SHA",
        );
        return false;
    }
    true
}

fn prepare_landing_proof(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    target: &str,
    need_id: &str,
    binding: &NeedAuditBinding,
    outcome: &jobs::JobOutcome,
) -> Result<Option<LandingProof>, VivariumError> {
    let Some((publication, result)) = worker_result(ctx, st, job, need_id, outcome)? else {
        return Ok(None);
    };
    let Some(accepted) = current_acceptance(ctx, st, job, target, need_id, binding)? else {
        return Ok(None);
    };
    let Some(target_ctx) = ctx.for_target(target) else {
        hold_landing(
            ctx,
            job,
            need_id,
            "configured target disappeared before publication verification",
        );
        return Ok(None);
    };
    let target_ref = format!("refs/heads/{}", target_ctx.cfg.branch);
    let merge_result_valid = match valid_merge_result(&result, job, binding) {
        Ok(valid) => valid,
        Err(error) => {
            hold_landing(
                ctx,
                job,
                need_id,
                &format!("durable need-merge job checks are malformed: {error}"),
            );
            return Ok(None);
        }
    };
    if !merge_result_valid || !valid_publish_proof(job, &publication, &result, binding, &target_ref)
    {
        hold_landing(
            ctx,
            job,
            need_id,
            "Merge-role result or runner publication does not match the accepted frozen range",
        );
        return Ok(None);
    }
    Ok(Some(LandingProof {
        accepted,
        binding: binding.clone(),
        target_ref,
        result,
        publication,
    }))
}

fn worker_result(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    need_id: &str,
    outcome: &jobs::JobOutcome,
) -> Result<Option<(crate::pump::jobs::RunnerPublishReceipt, NeedMergeResult)>, VivariumError> {
    let Some(publication) = outcome.runner_publish() else {
        hold_landing(
            ctx,
            job,
            need_id,
            "Merge role completed without a privileged runner publication receipt",
        );
        return Ok(None);
    };
    if !publication.succeeded() {
        fail_landing(
            ctx,
            st,
            job,
            "privileged runner did not publish the need-merge candidate",
        )?;
        return Ok(None);
    }
    let result = match parse_merge_result(&outcome.receipt) {
        Ok((result, form)) => {
            if form == ResultForm::Unprefixed {
                note_unprefixed(ctx, &job.id, MERGE_RESULT);
            }
            result
        }
        Err(error) => {
            hold_landing(
                ctx,
                job,
                need_id,
                &format!("published candidate has no valid PUMP_MERGE_RESULT: {error}"),
            );
            return Ok(None);
        }
    };
    Ok(Some((publication, result)))
}

fn current_acceptance(
    ctx: &Ctx,
    st: &State,
    job: &PoolJob,
    target: &str,
    need_id: &str,
    binding: &NeedAuditBinding,
) -> Result<Option<AcceptedNeedReceipt>, VivariumError> {
    let Some(accepted) = need_verdict::current_accepted_receipt(ctx, st, need_id, target) else {
        hold_landing(
            ctx,
            job,
            need_id,
            "accepted verdict is no longer current; preserving the published branch",
        );
        return Ok(None);
    };
    let Some(current) = need::binding_for(st, target, need_id)? else {
        hold_landing(
            ctx,
            job,
            need_id,
            "current frozen need binding is unavailable; preserving the published branch",
        );
        return Ok(None);
    };
    if &current != binding || &accepted.receipt.binding != binding {
        hold_landing(
            ctx,
            job,
            need_id,
            "current acceptance does not match the frozen branch binding",
        );
        return Ok(None);
    }
    Ok(Some(accepted))
}

fn verify_published_target(
    ctx: &Ctx,
    job: &PoolJob,
    need_id: &str,
    proof: &LandingProof,
) -> Option<String> {
    let target = ctx.for_target(&proof.binding.target)?;
    let Some(observed) = remote_ref_sha(&target, &proof.target_ref) else {
        ctx.log(&format!(
            "need landing verification deferred {}: target SHA unavailable",
            job.id
        ));
        return None;
    };
    if observed.as_deref() != Some(proof.publication.remote_sha.as_str()) {
        hold_landing(
            ctx,
            job,
            need_id,
            "current observed target SHA differs from the runner publication receipt",
        );
        return None;
    }
    let Some(sha) = observed else {
        hold_landing(ctx, job, need_id, "published target ref is absent");
        return None;
    };
    Some(sha)
}

fn record_verified_landing(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    proof: LandingProof,
    observed_target_sha: &str,
) -> Result<(), VivariumError> {
    let receipt = NeedLandingReceipt {
        binding: proof.binding.clone(),
        acceptance_receipt_key: proof.accepted.receipt.receipt_key.clone(),
        acceptance_job_id: proof.accepted.job_id.clone(),
        verdict: verdict_name(&proof.accepted)?,
        landing_job_id: job.id.clone(),
        target_ref: proof.target_ref.clone(),
        candidate_sha: proof.result.candidate.clone(),
        runner_published_sha: proof.publication.remote_sha.clone(),
        observed_target_sha: observed_target_sha.to_owned(),
        accepted_tip_in_candidate: proof.publication.contains_verified,
        candidate_in_observed_target: true,
        merge_report: job
            .outcome
            .as_ref()
            .map_or_else(String::new, |outcome| outcome.receipt.clone()),
    };
    need::record_landing(
        st,
        &proof.binding.target,
        &proof.binding.need,
        &job.id,
        receipt,
    )?;
    jobs::mark_merged(st, std::slice::from_ref(&job.id), observed_target_sha);
    ctx.log(&format!(
        "need landing verified {} target={} sha={}",
        proof.binding.need,
        proof.binding.target,
        short_sha(observed_target_sha)
    ));
    Ok(())
}

fn valid_publish_proof(
    job: &PoolJob,
    publication: &crate::pump::jobs::RunnerPublishReceipt,
    result: &NeedMergeResult,
    binding: &NeedAuditBinding,
    target_ref: &str,
) -> bool {
    job.kind == "need-merge"
        && job.target.as_deref() == Some(binding.target.as_str())
        && job
            .from_ref
            .as_deref()
            .is_some_and(|from| target_ref.strip_prefix("refs/heads/") == Some(from))
        && job
            .payload
            .get("target_ref")
            .and_then(serde_json::Value::as_str)
            == Some(target_ref)
        && job.publish_ref.as_deref() == Some(target_ref)
        && job.publish_expect.is_none()
        && job.publish_contains.as_deref() == Some(binding.tip.as_str())
        && publication.reference == target_ref
        && publication.candidate == result.candidate
        && publication.remote_sha == result.candidate
        && publication.contains.as_deref() == Some(binding.tip.as_str())
        && publication.contains_verified
        && full_sha(&publication.remote_sha)
}

fn valid_merge_result(
    result: &NeedMergeResult,
    job: &PoolJob,
    binding: &NeedAuditBinding,
) -> Result<bool, String> {
    let expected = required_checks_from_job(job)?;
    let source_id = job
        .payload
        .get("source_job_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| String::from("missing source_job_id"))?;
    Ok(result.version == 1
        && result.target == binding.target
        && result.need.as_deref() == Some(binding.need.as_str())
        && result.revision == Some(binding.revision)
        && result.base.as_deref() == Some(binding.base.as_str())
        && result.tip == binding.tip
        && result.status == "merged"
        && full_sha(&result.candidate)
        && valid_need_source(&result.sources, source_id)
        && same_checks(&expected, &result.checks)
        && job.status == JobStatus::Finished
        && job.outcome.as_ref().is_some_and(|outcome| {
            outcome.state == "finished" && outcome.agent_success() == Some(true)
        }))
}

fn valid_need_source(sources: &[NeedMergeSource], source_id: &str) -> bool {
    matches!(sources, [source] if source.kind == "job"
        && source.id == source_id
        && source.status == "merged")
}

fn required_checks_from_job(job: &PoolJob) -> Result<Vec<RequiredCheck>, String> {
    let value = job
        .payload
        .get("required_checks")
        .ok_or_else(|| String::from("missing required_checks"))?;
    serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid required_checks: {error}"))
}

fn same_checks(expected: &[RequiredCheck], reported: &[NeedMergeCheck]) -> bool {
    if expected.len() != reported.len() {
        return false;
    }
    let mut seen = BTreeSet::new();
    expected.iter().all(|check| {
        reported.iter().any(|result| {
            result.name == check.name
                && result.command == check.command
                && result.status == "passed"
                && seen.insert(result.name.clone())
        })
    })
}

fn parse_merge_result(receipt: &str) -> Result<(NeedMergeResult, ResultForm), String> {
    find_marked(receipt, MERGE_RESULT)
        .ok_or_else(|| String::from("no parsable merge result line is present"))
}

fn landing_job_identity(job: &PoolJob) -> Result<(String, String, NeedAuditBinding), String> {
    let target = job
        .target
        .clone()
        .ok_or_else(|| String::from("missing target"))?;
    let value = job
        .payload
        .get("binding")
        .ok_or_else(|| String::from("missing binding"))?;
    let binding: NeedAuditBinding = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid binding: {error}"))?;
    if binding.target != target {
        return Err(String::from("binding target does not match job target"));
    }
    if !binding.validate() {
        return Err(String::from("binding fields are invalid"));
    }
    let need_id = binding.need.clone();
    Ok((target, need_id, binding))
}

fn fail_landing(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    detail: &str,
) -> Result<(), VivariumError> {
    let Ok((target, need_id, _)) = landing_job_identity(job) else {
        ctx.exception("need-merge-failed", &job.id, detail);
        jobs::mark_integration_failed(st, &job.id, detail);
        jobs::mark_applied(st, &job.id, ctx.now());
        return Ok(());
    };
    if need::get(st, &target, &need_id)?.is_some_and(|branch| {
        branch.landing_job_id.as_deref() == Some(job.id.as_str())
            && branch.landing_receipt.is_none()
    }) {
        need::release_landing(st, &target, &need_id, &job.id, detail)?;
    }
    jobs::mark_integration_failed(st, &job.id, detail);
    jobs::mark_applied(st, &job.id, ctx.now());
    ctx.exception("need-merge-failed", &need_id, detail);
    Ok(())
}

fn hold_landing(ctx: &Ctx, job: &PoolJob, need_id: &str, detail: &str) {
    ctx.exception("need-landing-unverified", need_id, detail);
    ctx.log(&format!("need landing held {}: {detail}", job.id));
}

fn collect_cleanup(ctx: &Ctx, st: &mut State, job: &PoolJob) -> Result<(), VivariumError> {
    let Some((target_id, need_id, binding)) = cleanup_identity(ctx, st, job)? else {
        return Ok(());
    };
    let Some(branch) = need::get(st, &target_id, &need_id)? else {
        finish_cleanup_failure(
            ctx,
            st,
            job,
            "need branch record disappeared before cleanup collection",
            false,
        )?;
        return Ok(());
    };
    if branch.cleanup_receipt.is_some() {
        return Ok(());
    }
    if branch.cleanup_job_id.as_deref() != Some(job.id.as_str()) {
        hold_landing(
            ctx,
            job,
            &need_id,
            "terminal cleanup job does not own the branch cleanup reservation",
        );
        return Ok(());
    }
    let Some(landing) = branch.landing_receipt.as_ref() else {
        finish_cleanup_failure(
            ctx,
            st,
            job,
            "cleanup job has no verified landing receipt",
            false,
        )?;
        return Ok(());
    };
    if landing.binding != binding {
        hold_landing(
            ctx,
            job,
            &need_id,
            "cleanup binding differs from the verified landing receipt",
        );
        return Ok(());
    }
    let Some(outcome) = job.outcome.as_ref() else {
        return Ok(());
    };
    collect_cleanup_deletion(
        ctx, st, job, &target_id, &need_id, &branch, landing, outcome,
    )
}

fn cleanup_identity(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
) -> Result<Option<(String, String, NeedAuditBinding)>, VivariumError> {
    match landing_job_identity(job) {
        Ok(identity) => Ok(Some(identity)),
        Err(error) => {
            finish_cleanup_failure(
                ctx,
                st,
                job,
                &format!("need-cleanup job payload has an invalid frozen binding: {error}"),
                false,
            )?;
            Ok(None)
        }
    }
}

fn collect_cleanup_deletion(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    target_id: &str,
    need_id: &str,
    branch: &NeedBranch,
    landing: &NeedLandingReceipt,
    outcome: &jobs::JobOutcome,
) -> Result<(), VivariumError> {
    let Some(deletion) = outcome.runner_delete() else {
        finish_cleanup_failure(
            ctx,
            st,
            job,
            "cleanup job has no privileged runner delete receipt",
            true,
        )?;
        return Ok(());
    };
    let branch_ref = format!("refs/heads/{}", branch.branch);
    let expected_sha = landing.binding.tip.as_str();
    if !valid_delete_receipt(job, &deletion, &branch_ref, expected_sha) {
        let reason = format!(
            "runner did not confirm exact owned-ref deletion: {}",
            deletion.state
        );
        let retryable = !matches!(deletion.state.as_str(), "ref_mismatch" | "invalid_ref")
            && deletion.reference == branch_ref
            && deletion.expected == expected_sha;
        finish_cleanup_failure(ctx, st, job, &reason, retryable)?;
        return Ok(());
    }
    finish_verified_delete(
        ctx,
        st,
        job,
        target_id,
        need_id,
        &branch_ref,
        expected_sha,
        &deletion,
    )
}

fn finish_verified_delete(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    target_id: &str,
    need_id: &str,
    branch_ref: &str,
    expected_sha: &str,
    deletion: &crate::pump::jobs::RunnerDeleteReceipt,
) -> Result<(), VivariumError> {
    let Some(target) = ctx.for_target(target_id) else {
        ctx.log(&format!(
            "need cleanup verification deferred {}: target unavailable",
            job.id
        ));
        return Ok(());
    };
    match remote_ref_sha(&target, branch_ref) {
        None => {
            ctx.log(&format!(
                "need cleanup verification deferred {}: branch ref unavailable",
                job.id
            ));
            Ok(())
        }
        Some(Some(sha)) => preserve_existing_branch(ctx, st, job, expected_sha, &sha),
        Some(None) => record_verified_cleanup(
            ctx,
            st,
            job,
            target_id,
            need_id,
            branch_ref,
            expected_sha,
            &deletion,
        ),
    }
}

fn preserve_existing_branch(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    expected_sha: &str,
    observed_sha: &str,
) -> Result<(), VivariumError> {
    if observed_sha != expected_sha {
        finish_cleanup_failure(
            ctx,
            st,
            job,
            "need branch ref changed; preserving the newer branch tip",
            false,
        )
    } else {
        finish_cleanup_failure(
            ctx,
            st,
            job,
            "runner reported deletion but the exact need branch still exists",
            true,
        )
    }
}

fn valid_delete_receipt(
    job: &PoolJob,
    deletion: &crate::pump::jobs::RunnerDeleteReceipt,
    branch_ref: &str,
    expected_sha: &str,
) -> bool {
    job.kind == "need-cleanup"
        && job.status == JobStatus::Finished
        && job.outcome.as_ref().is_some_and(|outcome| {
            outcome.state == "finished" && outcome.agent_success() == Some(true)
        })
        && job.delete_ref.as_deref() == Some(branch_ref)
        && job.delete_expect.as_deref() == Some(expected_sha)
        && deletion.reference == branch_ref
        && deletion.expected == expected_sha
        && matches!(deletion.state.as_str(), "deleted" | "already-absent")
        && (deletion.state != "deleted" || deletion.before == expected_sha)
        && (deletion.state != "already-absent" || deletion.before == "absent")
        && deletion.after == "absent"
        && deletion.succeeded()
}

fn record_verified_cleanup(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    target: &str,
    need_id: &str,
    branch_ref: &str,
    expected_sha: &str,
    deletion: &crate::pump::jobs::RunnerDeleteReceipt,
) -> Result<(), VivariumError> {
    let receipt = NeedCleanupReceipt {
        cleanup_job_id: job.id.clone(),
        branch_ref: branch_ref.to_owned(),
        expected_sha: expected_sha.to_owned(),
        runner_state: deletion.state.clone(),
        before_sha: (deletion.before != "absent").then(|| deletion.before.clone()),
        verified_absent: deletion.after == "absent",
        completed_at: ctx.now(),
    };
    need::finish_cleanup(st, target, need_id, &job.id, receipt)?;
    jobs::mark_applied(st, &job.id, ctx.now());
    ctx.log(&format!(
        "need branch cleanup verified {need_id} ref={branch_ref}"
    ));
    Ok(())
}

fn finish_cleanup_failure(
    ctx: &Ctx,
    st: &mut State,
    job: &PoolJob,
    detail: &str,
    retryable: bool,
) -> Result<(), VivariumError> {
    let Ok((target, need_id, _)) = landing_job_identity(job) else {
        ctx.exception("need-cleanup-failed", &job.id, detail);
        jobs::mark_applied(st, &job.id, ctx.now());
        return Ok(());
    };
    need::release_cleanup_job(st, &target, &need_id, &job.id, detail, retryable)?;
    jobs::mark_applied(st, &job.id, ctx.now());
    ctx.exception("need-cleanup-failed", &need_id, detail);
    Ok(())
}

fn settle_gated_landings(ctx: &Ctx, st: &mut State) {
    let ids: Vec<String> = st
        .jobs
        .values()
        .filter(|job| {
            job.kind == "need-merge"
                && job.status.is_terminal()
                && job.applied_at.is_none()
                && job.gated_tip.is_some()
        })
        .map(|job| job.id.clone())
        .collect();
    for id in ids {
        jobs::mark_applied(st, &id, ctx.now());
    }
}

fn remote_ref_sha(ctx: &Ctx, reference: &str) -> Option<Option<String>> {
    let output = ctx.git(
        &format!(
            "ls-remote {} {}",
            shell_quote(&ctx.cfg.remote),
            shell_quote(reference)
        ),
        30,
    );
    if output.code != 0 {
        return None;
    }
    let mut matches = output.text.lines().filter_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name == reference).then_some(sha)
    });
    let Some(sha) = matches.next() else {
        return Some(None);
    };
    if matches.next().is_some() || !full_sha(sha) {
        return None;
    }
    Some(Some(sha.to_owned()))
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(10).collect()
}
