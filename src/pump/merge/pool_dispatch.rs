//! Pool-backed merge jobs. The pump selects branches and records their owner;
//! a merge-role worker performs the merge, checks and push in its pool slot.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::error::VivariumError;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{self, BranchSource, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::state::{MergeResult, State, Status};

use super::apply::{apply_failure, apply_undecided};
use super::lane::{eligible, recheck_held, take_batch};

const MERGE_BRIEF: &str = r#"You are the configured Vivi merge role. The pump selected these source branches and this target. Integrate the sources in the listed order in this checkout. The checkout starts on a unique scratch branch at the current integration tip. Keep the scratch branch; do not switch branches.

Fetch and merge each source branch. Resolve ordinary conflicts within the branch write scopes. For configured generated paths, take the target side on conflict, run its configured regeneration command, and continue. Run the configured formatter, every shared check, and each unit proof. When a failing proof matches a configured generated-file drift signature, regenerate and retry that proof once.

Classify the result honestly. If the source branch merges cleanly against the refreshed target but conflicts only because another integration job moved the target, return `undecided` with `why=lane-race`. A timeout, index lock, provider problem, or conflict during runner publication is also `undecided`; it is not a unit defect and must not consume a hand attempt. A conflict or failing proof that is isolated to a source is `dropped` only after you have diagnosed it. If a publish or re-merge conflict is only in a configured generated/counter path after green checks, refresh the target, regenerate or re-ask, and retry without charging the source. After all checks pass, commit the final integration tree yourself and record its full candidate SHA. Do not push. After your clean exit, the privileged pool runner publishes the exact candidate to the configured integration ref. The controller accepts `merged` only when the runner receipt names this candidate and the host independently verifies it on the remote integration ref. Never force, reset, or claim remote publication. The JSON `tip` is the candidate commit, not a claim about the later runner result.

End your receipt with one minified JSON line prefixed exactly `PUMP_MERGE_RESULT=`. Use this shape: `{"version":1,"target":"<target>","tip":"<full candidate SHA>","sources":[{"kind":"unit","id":"<unit id>","status":"merged|dropped|undecided","why":"<code or empty>","detail":"<diagnosis>","outside":[],"secs":0},{"kind":"job","id":"<origin job id>","status":"merged|dropped|undecided","why":"<code or empty>","detail":"<diagnosis>","outside":[],"secs":0}]}`. Include every requested source exactly once. `outside` is the repository paths changed outside a unit's write scope; use an empty list for role branches. The result tip is the full SHA you committed for runner publication."#;

#[derive(Debug, Clone, Deserialize)]
struct MergeReceipt {
    version: u32,
    target: String,
    tip: String,
    sources: Vec<SourceResult>,
}

#[derive(Debug, Clone, Deserialize)]
struct SourceResult {
    kind: String,
    id: String,
    status: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    outside: Vec<String>,
    #[serde(default)]
    secs: Option<u64>,
}

/// Enqueue at most one serialized merge job per repository target.
pub(crate) fn dispatch_pool(ctx: &Ctx, st: &mut State) -> Result<Vec<String>, VivariumError> {
    let role = ctx.routing.merge().filter(|role| {
        role.status == "active"
            && role
                .pool
                .as_ref()
                .is_some_and(|pool| ctx.routing.pools.contains_key(pool))
    });
    let Some(role) = role.cloned() else {
        ctx.exception(
            "merge-route-missing",
            &ctx.target_id,
            "the pump needs an active merge role assigned to a configured pool to queue durable merge jobs",
        );
        return Ok(Vec::new());
    };
    let targets = ctx.cfg.target_configs();
    let batches: BTreeMap<String, usize> = targets
        .iter()
        .map(|(id, target)| {
            (
                id.clone(),
                target
                    .merge
                    .as_ref()
                    .map_or(1, |merge| merge.batch as usize),
            )
        })
        .collect();
    recheck_held(st, &batches);
    let known: BTreeSet<String> = batches.keys().cloned().collect();
    let mut waiting = eligible(ctx, st, &known);
    let mut started = Vec::new();
    for target_id in targets.keys() {
        if merge_blocked(ctx, st, target_id) {
            continue;
        }
        let Some(target_ctx) = ctx.for_target(target_id) else {
            continue;
        };
        let size = batches.get(target_id).copied().unwrap_or(1).max(1);
        let units = take_batch(st, waiting.entry(target_id.clone()).or_default(), size);
        let solo = units.len() == 1 && st.units.get(&units[0]).is_some_and(|unit| unit.solo);
        let branches = if solo || units.len() >= size {
            Vec::new()
        } else {
            jobs::branch_sources_pending(st, target_id)
                .into_iter()
                .take(size - units.len())
                .collect()
        };
        let units = retain_branchable(ctx, st, units);
        if units.is_empty() && branches.is_empty() {
            continue;
        }
        let spec = merge_spec(st, &target_ctx, &role.name, units, branches)?;
        let id = spec.id.clone();
        jobs::enqueue(st, spec)?;
        if let Some(job) = st.jobs.get(&id) {
            for uid in &job.units {
                if let Some(unit) = st.units.get_mut(uid) {
                    unit.status = Status::Merging;
                }
            }
        }
        ctx.log(&format!("merge job queued {id} target={target_id}"));
        started.push(id);
    }
    Ok(started)
}

/// Apply terminal dispatched merge results once and preserve role jobs until
/// their merge and gate receipts are both present.
pub(crate) fn apply_dispatched(ctx: &Ctx, st: &mut State) {
    let jobs: Vec<PoolJob> = st
        .jobs
        .values()
        .filter(|job| job.kind == "merge" && job.status.is_terminal() && job.applied_at.is_none())
        .cloned()
        .collect();
    for job in jobs {
        apply_job(ctx, st, &job);
    }
}

fn merge_blocked(ctx: &Ctx, st: &mut State, target: &str) -> bool {
    let active_job = jobs::has_unsettled_kind_for_target(st, "merge", target);
    let waiting_gate = !jobs::sources_waiting_for_gate(st, target).is_empty();
    let waiting_unit_gate = crate::pump::state::with_target_state(st, target, |state| {
        state.last_gate.task.is_some() || state.last_gate.merges < state.merges
    });
    active_job || waiting_gate || waiting_unit_gate
}

fn retain_branchable(ctx: &Ctx, st: &mut State, units: Vec<String>) -> Vec<String> {
    units
        .into_iter()
        .filter(|uid| {
            let missing = st.units.get(uid).is_some_and(|unit| unit.branch.is_none());
            if missing {
                if let Some(unit) = st.units.get_mut(uid) {
                    unit.status = Status::Held;
                }
                ctx.exception(
                    "merge-branch-missing",
                    uid,
                    "delivered unit has no source branch; it cannot be merged",
                );
            }
            !missing
        })
        .collect()
}

fn merge_spec(
    st: &State,
    target: &Ctx,
    role: &str,
    units: Vec<String>,
    branches: Vec<BranchSource>,
) -> Result<JobSpec, VivariumError> {
    let id = next_merge_id(st, target.target_id.as_str());
    let runner_branch = format!(
        "pump/merge/{}/{}",
        safe_target(target.target_id.as_str()),
        id
    );
    let unit_input = unit_sources(st, &units)?;
    let payload = merge_payload(target, &unit_input, &branches, &runner_branch);
    let mut spec = JobSpec::new(
        id,
        "merge",
        role,
        &format!("merge batch for {}", target.target_id),
        &format!("{MERGE_BRIEF}\n\nInput JSON:\n{payload}"),
    );
    spec.target = Some(target.target_id.clone());
    spec.from_ref = Some(target.cfg.branch.clone());
    spec.branch = Some(runner_branch);
    spec.publish_ref = Some(format!("refs/heads/{}", target.cfg.branch));
    spec.budget = target
        .cfg
        .merge
        .remote_limit
        .max(target.cfg.dispatch.budget);
    spec.units = units.clone();
    spec.source_jobs = branches
        .iter()
        .map(|source| source.job_id.clone())
        .collect();
    spec.with = units
        .iter()
        .filter_map(|uid| st.units.get(uid).and_then(|unit| unit.branch.clone()))
        .chain(branches.iter().map(|source| source.branch.clone()))
        .collect();
    Ok(spec)
}

fn unit_sources(st: &State, units: &[String]) -> Result<Vec<serde_json::Value>, VivariumError> {
    let sources: Vec<serde_json::Value> = units
        .iter()
        .filter_map(|uid| st.units.get(uid).map(|unit| (uid, unit)))
        .map(|(uid, unit)| {
            serde_json::json!({
                "id": uid,
                "need": unit.need,
                "branch": unit.branch,
                "title": unit.title,
                "proof": unit.proof,
                "write_scope": unit.write_scope,
                "kind": unit.tools,
            })
        })
        .collect();
    if sources.len() == units.len() {
        return Ok(sources);
    }
    Err(VivariumError::Message(
        "a selected merge unit disappeared before its job was queued".into(),
    ))
}

fn merge_payload(
    target: &Ctx,
    units: &[serde_json::Value],
    branches: &[BranchSource],
    runner_branch: &str,
) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "target": target.target_id,
        "integration_branch": target.cfg.branch,
        "integration_ref": format!("refs/heads/{}", target.cfg.branch),
        "integration_remote": target.cfg.remote,
        "runner_branch": runner_branch,
        "base_ref": target.cfg.branch,
        "unit_sources": units,
        "role_sources": branches.iter().map(|source| serde_json::json!({
            "job_id": source.job_id,
            "role": source.role,
            "branch": source.branch,
            "subject": source.subject,
        })).collect::<Vec<_>>(),
        "format": target.cfg.merge.format,
        "shared_checks": target.cfg.merge.shared_checks,
        "checks_for": target.cfg.merge.checks_for,
        "generated": target.cfg.merge.generated,
    })
}

fn next_merge_id(st: &impl JobStore, target: &str) -> String {
    let prefix = format!("merge-{}-", safe_target(target));
    let next = st
        .jobs()
        .keys()
        .filter_map(|id| id.strip_prefix(&prefix)?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    format!("{prefix}{next}")
}

fn safe_target(target: &str) -> String {
    target
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn apply_job(ctx: &Ctx, st: &mut State, job: &PoolJob) {
    let expected_units: BTreeSet<String> = job.units.iter().cloned().collect();
    let expected_jobs: BTreeSet<String> = job.source_jobs.iter().cloned().collect();
    let Some(outcome) = job.outcome.as_ref().filter(|outcome| {
        job.status == JobStatus::Finished
            && outcome.state == "finished"
            && outcome.agent_success() == Some(true)
    }) else {
        reject_job(
            ctx,
            st,
            job,
            "merge-worker-failed",
            "pool worker did not report a clean exit",
        );
        return;
    };
    let Some(receipt) = parse_receipt(&outcome.receipt) else {
        reject_job(
            ctx,
            st,
            job,
            "merge-result-invalid",
            "merge worker receipt has no valid PUMP_MERGE_RESULT JSON",
        );
        return;
    };
    if !valid_result(&receipt, job, outcome, &expected_units, &expected_jobs) {
        reject_job(
            ctx,
            st,
            job,
            "merge-result-invalid",
            "merge receipt does not match the dispatched sources or published tip",
        );
        return;
    }
    let Some(integration_tip) = published_integration_tip(ctx, job, outcome, &receipt.tip) else {
        reject_job(
            ctx,
            st,
            job,
            "merge-publish-unverified",
            "merge candidate is not verified on the configured integration remote",
        );
        return;
    };
    let merged_jobs: Vec<String> = receipt
        .sources
        .iter()
        .filter(|source| source.kind == "job" && source.status == "merged")
        .map(|source| source.id.clone())
        .collect();
    for source in &receipt.sources {
        apply_source(ctx, st, job, source, &integration_tip);
    }
    jobs::mark_merged(st, &merged_jobs, &integration_tip);
    jobs::mark_applied(st, &job.id, ctx.now());
}

fn valid_result(
    receipt: &MergeReceipt,
    job: &PoolJob,
    outcome: &jobs::JobOutcome,
    expected_units: &BTreeSet<String>,
    expected_jobs: &BTreeSet<String>,
) -> bool {
    receipt.version == 1
        && receipt.target == job.target.as_deref().unwrap_or_default()
        && valid_sha(&receipt.tip)
        && valid_sources(&receipt.sources, expected_units, expected_jobs)
        && outcome.agent_success() == Some(true)
}

fn reject_job(ctx: &Ctx, st: &mut State, job: &PoolJob, why: &str, detail: &str) {
    undecided_job(ctx, st, job, why, detail);
    jobs::mark_applied(st, &job.id, ctx.now());
}

fn apply_source(ctx: &Ctx, st: &mut State, job: &PoolJob, source: &SourceResult, tip: &str) {
    let secs = source.secs.unwrap_or(0);
    let result = MergeResult {
        uid: source.id.clone(),
        ok: Some(source.status == "merged"),
        tip: Some(tip.to_owned()),
        outside: source.outside.clone(),
        secs: Some(secs),
        why: (!source.why.is_empty()).then(|| source.why.clone()),
        detail: (!source.detail.is_empty()).then(|| source.detail.clone()),
        ..MergeResult::default()
    };
    match (source.kind.as_str(), source.status.as_str()) {
        ("unit", "merged") if job.units.contains(&source.id) => {
            super::apply::apply_merged_dispatched(ctx, st, &result);
        }
        ("unit", "dropped") if job.units.contains(&source.id) => apply_failure(ctx, st, &result),
        ("unit", "undecided") if job.units.contains(&source.id) => {
            mark_solo_retry(st, &source.id, &source.why);
            apply_undecided(ctx, st, &result);
        }
        ("job", "dropped") if job.source_jobs.contains(&source.id) => {
            jobs::mark_integration_failed(st, &source.id, &source.detail);
            ctx.exception("merge-source-failed", &source.id, &source.detail);
        }
        ("job", "undecided") if job.source_jobs.contains(&source.id) => {
            ctx.log(&format!("merge-source-retry {}: {}", source.id, source.why));
        }
        ("job", "merged") if job.source_jobs.contains(&source.id) => {}
        _ => ctx.log(&format!(
            "merge result ignored {}:{}",
            source.kind, source.id
        )),
    }
}

fn mark_solo_retry(st: &mut State, uid: &str, why: &str) {
    if matches!(
        why,
        "lane-race" | "generated-publish-conflict" | "publish-conflict"
    ) && let Some(unit) = st.units.get_mut(uid)
    {
        unit.solo = true;
    }
}

fn undecided_job(ctx: &Ctx, st: &mut State, job: &PoolJob, why: &str, detail: &str) {
    for uid in &job.units {
        let result = MergeResult {
            uid: uid.clone(),
            why: Some(why.to_owned()),
            detail: Some(detail.to_owned()),
            ..MergeResult::default()
        };
        apply_undecided(ctx, st, &result);
    }
    ctx.exception("merge-job-undecided", &job.id, detail);
}

fn parse_receipt(receipt: &str) -> Option<MergeReceipt> {
    let line = receipt
        .lines()
        .find_map(|line| line.trim().strip_prefix("PUMP_MERGE_RESULT="))?;
    serde_json::from_str(line).ok()
}

fn valid_sources(
    sources: &[SourceResult],
    units: &BTreeSet<String>,
    jobs: &BTreeSet<String>,
) -> bool {
    let mut seen_units = BTreeSet::new();
    let mut seen_jobs = BTreeSet::new();
    for source in sources {
        if !matches!(source.status.as_str(), "merged" | "dropped" | "undecided") {
            return false;
        }
        if source.status != "merged" && (source.why.is_empty() || source.detail.is_empty()) {
            return false;
        }
        match source.kind.as_str() {
            "unit" if units.contains(&source.id) => {
                if !seen_units.insert(source.id.clone()) {
                    return false;
                }
            }
            "job" if jobs.contains(&source.id) => {
                if !seen_jobs.insert(source.id.clone()) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    &seen_units == units && &seen_jobs == jobs
}

fn published_integration_tip(
    ctx: &Ctx,
    job: &PoolJob,
    outcome: &jobs::JobOutcome,
    candidate: &str,
) -> Option<String> {
    let target = ctx.for_target(job.target.as_deref()?)?;
    let branch = format!("refs/heads/{}", target.cfg.branch);
    if job.publish_ref.as_deref() != Some(branch.as_str()) || job.publish_expect.is_some() {
        return None;
    }
    let publication = outcome.runner_publish()?;
    if !publication.succeeded()
        || publication.reference != branch
        || publication.candidate != candidate
    {
        return None;
    }
    let fetch = target.git(
        &format!(
            "fetch --quiet {} {}",
            shell_quote(&target.cfg.remote),
            shell_quote(&branch)
        ),
        120,
    );
    if fetch.code != 0 {
        return None;
    }
    let head = target.git("rev-parse --verify --quiet FETCH_HEAD^{commit}", 30);
    if head.code != 0 {
        return None;
    }
    let integration_tip = head.text.trim();
    if !valid_sha(integration_tip) {
        return None;
    }
    let ancestry = target.git(
        &format!("merge-base --is-ancestor {candidate} {integration_tip}"),
        30,
    );
    (ancestry.code == 0 && publication.remote_sha == integration_tip)
        .then(|| integration_tip.to_owned())
}

fn valid_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "pool_dispatch_test.rs"]
mod tests;
