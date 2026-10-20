use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec};
use crate::pump::need::{
    self, CleanupRetryState, NeedAuditBinding, NeedBranch, NeedLandingReceipt,
};
use crate::pump::need_verdict::{self, AcceptedNeedReceipt};
use crate::pump::state::State;

use super::{RequiredCheck, full_sha, verdict_name};

pub(crate) fn dispatch(ctx: &Ctx, st: &mut State) -> Result<Vec<String>, VivariumError> {
    let branches = need::frozen(st)?;
    let mut started = Vec::new();
    let mut target_started = BTreeSet::new();
    for branch in branches {
        if st.closed_needs.contains(&branch.need) {
            continue;
        }
        if let Some(id) = dispatch_branch(ctx, st, &branch, &mut target_started)? {
            started.push(id);
        }
    }
    Ok(started)
}

fn dispatch_branch(
    ctx: &Ctx,
    st: &mut State,
    branch: &NeedBranch,
    target_started: &mut BTreeSet<String>,
) -> Result<Option<String>, VivariumError> {
    if branch.landing_receipt.is_some() {
        return dispatch_cleanup(ctx, st, branch);
    }
    if branch.landing_job_id.is_some() || branch.landing_error.is_some() {
        return Ok(None);
    }
    let Some(accepted) = current_acceptance(ctx, st, branch) else {
        return Ok(None);
    };
    let Some(binding) = current_binding(st, branch)? else {
        return Ok(None);
    };
    if binding != accepted.receipt.binding
        || target_started.contains(&branch.target)
        || crate::pump::merge::target_write_blocked(st, &branch.target)
    {
        return Ok(None);
    }
    let Some(id) = queue_landing(ctx, st, branch, &accepted, &binding)? else {
        return Ok(None);
    };
    target_started.insert(branch.target.clone());
    Ok(Some(id))
}

fn queue_landing(
    ctx: &Ctx,
    st: &mut State,
    branch: &NeedBranch,
    accepted: &AcceptedNeedReceipt,
    binding: &NeedAuditBinding,
) -> Result<Option<String>, VivariumError> {
    let Some(role) = merge_role(ctx) else {
        ctx.exception(
            "need-merge-route-missing",
            &branch.need,
            "accepted need branches need an active Merge role assigned to a configured pool",
        );
        return Ok(None);
    };
    let Some(target) = ctx.for_target(&branch.target) else {
        ctx.exception(
            "need-merge-target-missing",
            &branch.need,
            &format!("configured target {} could not be resolved", branch.target),
        );
        return Ok(None);
    };
    if target.cfg.branch.is_empty() || target.cfg.branch == target.cfg.main {
        ctx.exception(
            "need-merge-target-invalid",
            &branch.need,
            "need landing requires a configured integration branch distinct from main",
        );
        return Ok(None);
    }
    if !full_sha(&binding.base) || !full_sha(&binding.tip) {
        ctx.exception(
            "need-merge-binding-invalid",
            &branch.need,
            "frozen need range does not contain full commit IDs",
        );
        return Ok(None);
    }
    enqueue_landing(ctx, st, branch, accepted, binding, &target, &role.name)
}

fn enqueue_landing(
    ctx: &Ctx,
    st: &mut State,
    branch: &NeedBranch,
    accepted: &AcceptedNeedReceipt,
    binding: &NeedAuditBinding,
    target: &Ctx,
    role: &str,
) -> Result<Option<String>, VivariumError> {
    let spec = merge_spec(st, target, role, branch, accepted, binding)?;
    need::reserve_landing(st, &branch.target, &branch.need, binding, &spec.id)?;
    let id = spec.id.clone();
    if let Err(error) = jobs::enqueue(st, spec) {
        need::release_landing(
            st,
            &branch.target,
            &branch.need,
            &id,
            &format!("could not enqueue landing job: {error}"),
        )?;
        return Err(error);
    }
    ctx.log(&format!(
        "need landing queued {id} need={} target={}",
        branch.need, branch.target
    ));
    Ok(Some(id))
}

/// Select the active Merge role assigned to a configured pool.
fn merge_role(ctx: &Ctx) -> Option<crate::routing::RoutedRole> {
    ctx.routing
        .merge()
        .filter(|role| {
            role.status == "active"
                && role
                    .pool
                    .as_ref()
                    .is_some_and(|pool| ctx.routing.pools.contains_key(pool))
        })
        .cloned()
}

fn current_acceptance(ctx: &Ctx, st: &State, branch: &NeedBranch) -> Option<AcceptedNeedReceipt> {
    let accepted = need_verdict::current_accepted_receipt(ctx, st, &branch.need, &branch.target)?;
    let binding = match need::binding_for(st, &branch.target, &branch.need) {
        Ok(Some(binding)) => binding,
        Ok(None) => return None,
        Err(error) => {
            ctx.exception("need-landing-binding", &branch.need, &error.to_string());
            return None;
        }
    };
    (accepted.receipt.binding == binding && accepted_verdict(&accepted)).then_some(accepted)
}

fn current_binding(
    st: &State,
    branch: &NeedBranch,
) -> Result<Option<NeedAuditBinding>, VivariumError> {
    need::binding_for(st, &branch.target, &branch.need)
}

fn accepted_verdict(accepted: &AcceptedNeedReceipt) -> bool {
    matches!(
        accepted.receipt.verdict,
        crate::pump::need_audit::NeedAuditVerdict::CleanPass
            | crate::pump::need_audit::NeedAuditVerdict::Residual
    )
}

fn merge_spec(
    st: &State,
    target: &Ctx,
    role: &str,
    branch: &NeedBranch,
    accepted: &AcceptedNeedReceipt,
    binding: &NeedAuditBinding,
) -> Result<JobSpec, VivariumError> {
    let target_ref = format!("refs/heads/{}", target.cfg.branch);
    let required_checks = required_checks(st, target, binding);
    let source_job_id = format!("need-source-{}", binding_digest(binding)?);
    let frozen_units = frozen_unit_sources(st, branch)?;
    let payload = serde_json::json!({
        "version": 1,
        "binding": binding,
        "source_branch": branch.branch,
        "source_job_id": source_job_id,
        "frozen_units": frozen_units,
        "target_ref": target_ref,
        "acceptance_receipt_key": accepted.receipt.receipt_key,
        "acceptance_job_id": accepted.job_id,
        "verdict": verdict_name(accepted)?,
        "required_checks": required_checks,
        "generated": target.cfg.merge.generated.clone(),
        "attempt": 1,
    });
    let body = merge_brief(&payload)?;
    let id = job_id("need-merge", binding, 1)?;
    let scratch = format!(
        "vivi/need-land/{}/{}",
        safe_component(&binding.target),
        short_hash(&id)
    );
    let mut spec = JobSpec::new(
        id,
        "need-merge",
        role,
        &format!("land accepted need {} on {}", binding.need, binding.target),
        body,
    );
    spec.target = Some(binding.target.clone());
    spec.builtin_role = Some(String::from("merge"));
    spec.from_ref = Some(target.cfg.branch.clone());
    spec.with = vec![binding.tip.clone()];
    spec.source_jobs = vec![source_job_id];
    spec.units = branch.frozen_units.clone();
    spec.branch = Some(scratch);
    spec.publish_ref = Some(target_ref);
    spec.publish_contains = Some(binding.tip.clone());
    spec.integration_required = true;
    spec.budget = target
        .cfg
        .merge
        .remote_limit
        .max(target.cfg.dispatch.budget);
    spec.payload = payload;
    Ok(spec)
}

fn merge_brief(payload: &serde_json::Value) -> Result<String, VivariumError> {
    let encoded = serde_json::to_string_pretty(payload).map_err(|error| {
        VivariumError::Message(format!("cannot encode need landing input: {error}"))
    })?;
    Ok(format!(
        "You are the predefined Vivi Merge role handling a `need-merge` job. Start from the supplied integration target. Fetch the exact target_ref from the assigned remote and fast-forward your scratch checkout to its current published tip before merging; retain all already-integrated work. Then merge exactly the supplied frozen need branch, and do not substitute a newer source tip. Run the exact required checks, resolve conflicts when within scope, and commit the candidate. Do not push; the privileged pool runner publishes after your successful exit. Preserve the accepted need tip as an ancestor of the candidate. Report every required check exactly once with its exact name, command, and `passed` or `failed` status. End with one minified JSON line prefixed `PUMP_MERGE_RESULT=` containing version 1, target, need, revision, base, tip (the accepted frozen need tip), candidate (your full commit SHA), status (`merged` only when all checks pass), checks, and exactly one source object {{\"kind\":\"job\",\"id\":\"<source_job_id>\",\"status\":\"merged|undecided|dropped\",\"why\":\"...\",\"detail\":\"...\"}}. If any conflict or required check remains unresolved, use `undecided` or `dropped`, explain it, and exit unsuccessfully so the runner publishes nothing. Keep conflict resolutions and the final commit visible in your human-readable report.\n\nInput JSON:\n{encoded}\n"
    ))
}

fn frozen_unit_sources(
    st: &State,
    branch: &NeedBranch,
) -> Result<Vec<serde_json::Value>, VivariumError> {
    branch
        .frozen_units
        .iter()
        .map(|id| {
            let unit = st.units.get(id).ok_or_else(|| {
                VivariumError::Message(format!(
                    "frozen need branch {} names missing unit {id}",
                    branch.need
                ))
            })?;
            Ok(serde_json::json!({
                "id": id,
                "branch": unit.branch,
                "tip": unit.commit,
                "write_scope": unit.write_scope,
            }))
        })
        .collect()
}

fn required_checks(st: &State, target: &Ctx, binding: &NeedAuditBinding) -> Vec<RequiredCheck> {
    let merge = &target.cfg.merge;
    let mut checks = Vec::new();
    if !merge.format.trim().is_empty() {
        checks.push(RequiredCheck {
            name: String::from("format"),
            command: merge.format.clone(),
        });
    }
    checks.extend(
        merge
            .shared_checks
            .iter()
            .enumerate()
            .map(|(index, command)| RequiredCheck {
                name: format!("shared-{index}"),
                command: command.clone(),
            }),
    );
    let scopes = need_scopes(st, binding);
    for (pattern, commands) in &merge.checks_for {
        let path = vec![pattern.clone()];
        if scopes
            .iter()
            .any(|scope| crate::pump::plan::overlaps(scope, &path))
        {
            checks.extend(
                commands
                    .iter()
                    .enumerate()
                    .map(|(index, command)| RequiredCheck {
                        name: format!("scope-{pattern}-{index}"),
                        command: command.clone(),
                    }),
            );
        }
    }
    checks
}

fn need_scopes(st: &State, binding: &NeedAuditBinding) -> Vec<Vec<String>> {
    st.needs
        .get(&binding.need)
        .into_iter()
        .flat_map(|record| record.units.iter())
        .filter_map(|unit_id| st.units.get(unit_id))
        .filter(|unit| unit.target_id() == binding.target)
        .map(|unit| unit.write_scope.clone())
        .collect()
}

fn dispatch_cleanup(
    ctx: &Ctx,
    st: &mut State,
    branch: &NeedBranch,
) -> Result<Option<String>, VivariumError> {
    if branch.cleanup_receipt.is_some() || branch.cleanup_job_id.is_some() {
        return Ok(None);
    }
    let Some(landing) = branch.landing_receipt.as_ref() else {
        return Ok(None);
    };
    if !matches!(
        need::cleanup_retry_state(st, &branch.target, &branch.need)?,
        CleanupRetryState::Ready | CleanupRetryState::Retryable
    ) {
        return Ok(None);
    }
    let Some(spec) = cleanup_spec(ctx, st, branch, landing)? else {
        return Ok(None);
    };
    let id = spec.id.clone();
    need::record_cleanup_job(st, &branch.target, &branch.need, &id)?;
    if let Err(error) = jobs::enqueue(st, spec) {
        need::release_cleanup_job(
            st,
            &branch.target,
            &branch.need,
            &id,
            &error.to_string(),
            true,
        )?;
        return Err(error);
    }
    ctx.log(&format!(
        "need cleanup queued {id} need={} branch={}",
        branch.need, branch.branch
    ));
    Ok(Some(id))
}

fn cleanup_spec(
    ctx: &Ctx,
    st: &State,
    branch: &NeedBranch,
    landing: &NeedLandingReceipt,
) -> Result<Option<JobSpec>, VivariumError> {
    let Some((role, target, binding, branch_ref)) = cleanup_metadata(ctx, st, branch, landing)?
    else {
        return Ok(None);
    };
    let (id, attempt) = cleanup_job_id(st, &binding)?;
    let payload = serde_json::json!({
        "version": 1,
        "attempt": attempt,
        "binding": binding,
        "branch_ref": branch_ref,
        "expected_sha": landing.binding.tip,
        "landing_target_sha": landing.observed_target_sha,
        "landing_job_id": landing.landing_job_id,
    });
    let body = cleanup_brief(&payload)?;
    let mut spec = JobSpec::new(
        id,
        "need-cleanup",
        role.name,
        &format!("clean landed need branch {}", branch.need),
        body,
    );
    spec.target = Some(branch.target.clone());
    spec.builtin_role = Some(String::from("merge"));
    spec.from_ref = Some(landing.observed_target_sha.clone());
    spec.with = (landing.binding.tip != landing.observed_target_sha)
        .then(|| landing.binding.tip.clone())
        .into_iter()
        .collect();
    spec.branch = Some(format!("vivi/need-cleanup/{}", spec.id));
    spec.delete_ref = Some(branch_ref);
    spec.delete_expect = Some(landing.binding.tip.clone());
    spec.payload = payload;
    Ok(Some(spec))
}

fn cleanup_metadata(
    ctx: &Ctx,
    st: &State,
    branch: &NeedBranch,
    landing: &NeedLandingReceipt,
) -> Result<Option<(crate::routing::RoutedRole, Ctx, NeedAuditBinding, String)>, VivariumError> {
    let Some(role) = merge_role(ctx) else {
        ctx.exception(
            "need-cleanup-route-missing",
            &branch.need,
            "verified need landing needs an active Merge role assigned to a configured pool for branch cleanup",
        );
        return Ok(None);
    };
    let Some(target) = ctx.for_target(&branch.target) else {
        return Ok(None);
    };
    let Some(binding) = current_binding(st, branch)? else {
        return Ok(None);
    };
    if binding != landing.binding {
        ctx.exception(
            "need-cleanup-binding-stale",
            &branch.need,
            "frozen need branch binding changed after verified landing; preserving its branch",
        );
        return Ok(None);
    }
    let Some(branch_ref) = owned_branch_ref(&branch.branch) else {
        ctx.exception(
            "need-cleanup-ref-invalid",
            &branch.need,
            "refusing to delete a need branch outside vivi/need/<id>",
        );
        return Ok(None);
    };
    Ok(Some((role, target, binding, branch_ref)))
}
fn cleanup_brief(payload: &serde_json::Value) -> Result<String, VivariumError> {
    let encoded = serde_json::to_string_pretty(payload).map_err(|error| {
        VivariumError::Message(format!("cannot encode need cleanup input: {error}"))
    })?;
    Ok(format!(
        "This is a cleanup-only Merge-role job. Do not create, change, switch, merge, push, or delete any ref. Confirm the supplied landing receipt metadata, then exit successfully so the privileged runner can perform its exact compare-and-delete operation. The runner deletes only when the remote ref still has the supplied expected SHA. Do not claim deletion yourself.\n\nInput JSON:\n{encoded}\n"
    ))
}
fn cleanup_job_id(st: &State, binding: &NeedAuditBinding) -> Result<(String, u64), VivariumError> {
    let identity = binding_digest(binding)?;
    let encoded_binding = binding_value(binding)?;
    let prior = st
        .jobs
        .values()
        .filter(|job| {
            job.kind == "need-cleanup" && job.payload.get("binding") == Some(&encoded_binding)
        })
        .filter_map(|job| {
            job.payload
                .get("attempt")
                .and_then(serde_json::Value::as_u64)
        })
        .max()
        .unwrap_or(0);
    let attempt = prior.saturating_add(1);
    Ok((format!("need-cleanup-{identity}-a{attempt}"), attempt))
}

fn job_id(kind: &str, binding: &NeedAuditBinding, attempt: u64) -> Result<String, VivariumError> {
    Ok(format!("{kind}-{}-a{attempt}", binding_digest(binding)?))
}

fn binding_digest(binding: &NeedAuditBinding) -> Result<String, VivariumError> {
    let bytes = serde_json::to_vec(binding).map_err(|error| {
        VivariumError::Message(format!(
            "cannot encode need binding for job identity: {error}"
        ))
    })?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn binding_value(binding: &NeedAuditBinding) -> Result<serde_json::Value, VivariumError> {
    serde_json::to_value(binding)
        .map_err(|error| VivariumError::Message(format!("cannot encode need binding: {error}")))
}

fn safe_component(value: &str) -> String {
    let component: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if component.is_empty() {
        String::from("target")
    } else {
        component
    }
}

fn short_hash(value: &str) -> String {
    value
        .chars()
        .rev()
        .take(12)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn owned_branch_ref(branch: &str) -> Option<String> {
    (branch.starts_with("vivi/need/")
        && !branch.contains("..")
        && !branch.contains("//")
        && branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.')))
    .then(|| format!("refs/heads/{branch}"))
}
