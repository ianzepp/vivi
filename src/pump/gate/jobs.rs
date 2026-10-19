//! Durable gate-job collection, source gating, and promotion receipts.

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{completion, mark_applied, mark_gated};
use crate::pump::state::{Promotion, State};

use super::{finish_gate, tip_short, write_force_flag};

/// Mark source branches covered by a clean gate, including sources whose
/// integration tips are ancestors of the tested tip.
pub(super) fn mark_gated_sources(ctx: &Ctx, st: &mut State, ids: &[String], tip: &str) {
    let Some(remote_tip) = remote_branch_tip(ctx, &ctx.cfg.branch) else {
        ctx.log("gate source ancestry unavailable; role jobs remain pending");
        return;
    };
    if ancestor(ctx, tip, &remote_tip) != Some(true) {
        ctx.log("gate source tip is no longer on the integration branch");
        return;
    }
    let covered: Vec<String> = ids
        .iter()
        .filter(|id| {
            st.jobs.get(*id).is_some_and(|job| {
                job.integration_required
                    && job.target.as_deref() == Some(ctx.target_id.as_str())
                    && job.gated_tip.is_none()
                    && job.integration_error.is_none()
                    && job
                        .merged_tip
                        .as_deref()
                        .is_some_and(|merged| ancestor(ctx, merged, tip) == Some(true))
            })
        })
        .cloned()
        .collect();
    mark_gated(st, &covered, tip);
}

/// Apply full receipts for every configured target.
pub fn collect_completed(ctx: &Ctx, st: &mut State) {
    let targets: Vec<String> = ctx.cfg.target_configs().keys().cloned().collect();
    for target in targets {
        let Some(target_ctx) = ctx.for_target(&target) else {
            continue;
        };
        crate::pump::state::with_target_state(st, &target, |target_state| {
            collect_gate_job(&target_ctx, target_state);
            collect_promotion_jobs(&target_ctx, target_state);
            retry_clean_gate_sources(&target_ctx, target_state);
        });
    }
}

/// Retry source bookkeeping after a clean gate when ancestry lookup was unavailable.
fn retry_clean_gate_sources(ctx: &Ctx, st: &mut State) {
    let clean_gates: Vec<(Vec<String>, String)> = st
        .jobs
        .values()
        .filter(|job| {
            job.kind == "gate"
                && job.target.as_deref() == Some(ctx.target_id.as_str())
                && job.applied_at.is_some()
                && job.outcome.as_ref().is_some_and(|outcome| {
                    outcome.state == "finished" && outcome.agent_success() == Some(true)
                })
                && !job.source_jobs.is_empty()
        })
        .filter_map(|job| {
            let tip = job.from_ref.as_ref()?;
            st.gates
                .iter()
                .any(|gate| gate.tip == *tip && gate.names.as_ref().is_some_and(Vec::is_empty))
                .then(|| (job.source_jobs.clone(), tip.clone()))
        })
        .collect();

    for (sources, tip) in clean_gates {
        let pending: Vec<String> = sources
            .into_iter()
            .filter(|id| {
                st.jobs.get(id).is_some_and(|job| {
                    job.integration_required
                        && job.merged_tip.is_some()
                        && job.gated_tip.is_none()
                        && job.integration_error.is_none()
                })
            })
            .collect();
        if !pending.is_empty() {
            mark_gated_sources(ctx, st, &pending, &tip);
        }
    }
}

/// Apply one target's gate receipt and release its durable gate job.
fn collect_gate_job(ctx: &Ctx, st: &mut State) {
    let Some(job_id) = st.last_gate.job_id.clone() else {
        return;
    };
    let Some(outcome) = completion(st, &job_id).cloned() else {
        return;
    };
    if !mark_applied(st, &job_id, ctx.now()) {
        return;
    }
    if let Some(job) = st.jobs.get(&job_id) {
        st.last_gate.task.clone_from(&job.task);
        st.last_gate.pool.clone_from(&job.pool);
    }
    if outcome.state != "finished" || outcome.agent_success() != Some(true) {
        let tip = st.last_gate.tip.clone().unwrap_or_default();
        ctx.exception(
            "gate-job-failed",
            &tip_short(&tip),
            &format!("{}: {}", outcome.state, outcome.detail),
        );
        st.last_gate.task = None;
        st.last_gate.job_id = None;
        write_force_flag(ctx);
        return;
    }
    finish_gate(ctx, st, &outcome.receipt);
    st.last_gate.job_id = None;
}

/// Apply terminal promotion receipts, verifying any claimed prior landing.
fn collect_promotion_jobs(ctx: &Ctx, st: &mut State) {
    let pending: Vec<String> = st
        .jobs
        .iter()
        .filter(|(_, job)| {
            job.kind == "promote"
                && job.target.as_deref() == Some(ctx.target_id.as_str())
                && job.outcome.is_some()
                && job.applied_at.is_none()
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in pending {
        apply_promotion(ctx, st, &id);
    }
}

/// Apply one terminal promotion job, recording main's failing set only after
/// a runner push receipt or an independent remote ancestry check.
fn apply_promotion(ctx: &Ctx, st: &mut State, id: &str) {
    let Some(job) = st.jobs.get(id).cloned() else {
        return;
    };
    let Some(outcome) = completion(st, id).cloned() else {
        return;
    };
    let branch = job
        .payload
        .get("branch")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(ctx.cfg.main.as_str());
    let tip = job
        .payload
        .get("tip")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    match promotion_landed(ctx, &job, &outcome, branch, tip) {
        Some(true) => {}
        Some(false) => {
            reject_promotion(
                ctx,
                st,
                id,
                tip,
                &format!("{}; expected {branch} to contain {tip}", outcome.detail),
            );
            return;
        }
        None => {
            ctx.log(&format!(
                "promote verification unavailable: {} at {}",
                tip_short(tip),
                branch
            ));
            return;
        }
    }
    record_promotion(
        ctx,
        st,
        id,
        tip,
        branch,
        outcome.at,
        job.payload.get("failing").and_then(json_names),
    );
}

/// Persist a verified promotion once and retain its failing-name set.
fn record_promotion(
    ctx: &Ctx,
    st: &mut State,
    id: &str,
    tip: &str,
    branch: &str,
    at: u64,
    failing: Option<Vec<String>>,
) {
    if !mark_applied(st, id, ctx.now()) {
        return;
    }
    if !st.promotions.iter().any(|promotion| promotion.tip == tip) {
        st.promotions.push(Promotion {
            tip: tip.to_owned(),
            at,
            failing,
        });
    }
    ctx.log(&format!(
        "promote confirmed: {branch} -> {}",
        tip_short(tip)
    ));
}

/// Mark an invalid terminal promotion receipt and report its evidence.
fn reject_promotion(ctx: &Ctx, st: &mut State, id: &str, tip: &str, detail: &str) {
    if mark_applied(st, id, ctx.now()) {
        ctx.exception("promote-job-failed", &tip_short(tip), detail);
    }
}

/// Resolve the marker claim against the durable runner push receipt or remote.
fn promotion_landed(
    ctx: &Ctx,
    job: &crate::pump::jobs::PoolJob,
    outcome: &crate::pump::jobs::JobOutcome,
    branch: &str,
    tip: &str,
) -> Option<bool> {
    let expected_ref = format!("refs/heads/{branch}");
    if outcome.agent_success() != Some(true)
        || job.publish_ref.as_deref() != Some(expected_ref.as_str())
        || job.publish_expect.as_deref() != Some(tip)
    {
        return Some(false);
    }
    let remote_tip = remote_branch_tip(ctx, branch)?;
    if !ancestor(ctx, tip, &remote_tip)? {
        return Some(false);
    }
    if let Some(publication) = outcome.runner_publish()
        && publication.succeeded()
        && publication.reference == expected_ref
        && publication.candidate == tip
        && full_sha(&publication.remote_sha)
        && !ancestor(ctx, &publication.remote_sha, &remote_tip)?
    {
        ctx.log(&format!(
            "promotion runner receipt was superseded at {branch}; remote contains {}",
            tip_short(tip)
        ));
    }
    Some(true)
}

/// Check whether the configured remote branch contains the requested tip.
/// Read the configured remote branch SHA when its commit is available locally.
fn remote_branch_tip(ctx: &Ctx, branch: &str) -> Option<String> {
    let reference = format!("refs/heads/{branch}");
    let out = ctx.git(
        &format!(
            "ls-remote {} {}",
            shell_quote(&ctx.cfg.remote),
            shell_quote(&reference)
        ),
        30,
    );
    if out.code != 0 {
        return None;
    }
    let sha = out.text.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name == reference && full_sha(sha)).then(|| sha.to_owned())
    })?;
    let object = format!("{sha}^{{commit}}");
    let exists = ctx.git(&format!("cat-file -e {object}"), 15);
    if exists.code == 0 {
        return Some(sha);
    }
    let fetched = ctx.git(
        &format!(
            "fetch --quiet --no-tags --no-write-fetch-head {} {}",
            shell_quote(&ctx.cfg.remote),
            shell_quote(&reference)
        ),
        30,
    );
    if fetched.code != 0 || ctx.git(&format!("cat-file -e {object}"), 15).code != 0 {
        return None;
    }
    Some(sha)
}

fn ancestor(ctx: &Ctx, candidate: &str, descendant: &str) -> Option<bool> {
    let output = ctx.git(
        &format!("merge-base --is-ancestor {candidate} {descendant}"),
        30,
    );
    match output.code {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// Whether `sha` is a full SHA-1 object name.
fn full_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Read the failing names from a durable promotion payload.
fn json_names(value: &serde_json::Value) -> Option<Vec<String>> {
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(String::from)
            .collect()
    })
}

/// Queue a fast-forward-only promotion on a role-addressed pool job.
pub(super) fn promote(ctx: &Ctx, st: &mut State, tip: &str, failing: &[String]) {
    if !ctx.cfg.promote || ctx.cfg.main.is_empty() {
        return;
    }
    match crate::pump::jobs::enqueue_promotion(ctx, st, &ctx.target_id, tip, failing) {
        Ok(Some(job)) => ctx.log(&format!(
            "promote queued: {} -> {} job={job}",
            ctx.cfg.main,
            tip_short(tip)
        )),
        Ok(None) => {}
        Err(err) => ctx.exception("promote-enqueue-failed", &tip_short(tip), &err.to_string()),
    }
}
