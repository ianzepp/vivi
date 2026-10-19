//! Durable gate-job collection, source gating, and promotion receipts.

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{completion, mark_applied};
use crate::pump::state::{Promotion, State};

use super::{finish_gate, tip_short, write_force_flag};

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
        });
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
    if outcome.state != "finished" {
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
    if outcome.state != "finished" {
        reject_promotion(
            ctx,
            st,
            id,
            tip,
            &format!("{}; promotion job did not finish", outcome.detail),
        );
        return;
    }
    match promotion_landed(ctx, &outcome, branch, tip) {
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
    outcome: &crate::pump::jobs::JobOutcome,
    branch: &str,
    tip: &str,
) -> Option<bool> {
    let Some((status, claimed_tip)) = promotion_marker(&outcome.receipt) else {
        return Some(false);
    };
    if claimed_tip != tip {
        return Some(false);
    }
    match status.as_str() {
        "pushed" if pushed_sha(outcome, branch).is_some_and(|sha| tip.starts_with(&sha)) => {
            Some(true)
        }
        "pushed" => remote_contains(ctx, branch, tip),
        "already-on-main" => remote_contains(ctx, branch, tip),
        _ => Some(false),
    }
}

/// Parse the exact machine-readable promotion marker from the worker receipt.
fn promotion_marker(receipt: &str) -> Option<(String, String)> {
    let line = receipt
        .lines()
        .rev()
        .find(|line| line.starts_with("PUMP_PROMOTE_RESULT="))?;
    let marker: serde_json::Value =
        serde_json::from_str(line.strip_prefix("PUMP_PROMOTE_RESULT=")?).ok()?;
    Some((
        marker.get("status")?.as_str()?.to_owned(),
        marker.get("tip")?.as_str()?.to_owned(),
    ))
}

/// Check whether the configured remote branch contains the requested tip.
fn remote_contains(ctx: &Ctx, branch: &str, tip: &str) -> Option<bool> {
    if !full_sha(tip) {
        return Some(false);
    }
    let tip_on_remote = remote_branch_tip(ctx, branch)?;
    let ancestry = ctx.git(
        &format!("merge-base --is-ancestor {tip} {tip_on_remote}"),
        30,
    );
    match ancestry.code {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

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

/// Extract a reconcile detail of the form `pushed <branch> <sha>`.
fn pushed_sha(outcome: &crate::pump::jobs::JobOutcome, branch: &str) -> Option<String> {
    let mut words = outcome.detail.split_whitespace();
    if words.next() != Some("pushed") || words.next() != Some(branch) {
        return None;
    }
    words
        .next()
        .filter(|sha| {
            (7..=40).contains(&sha.len()) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .map(str::to_owned)
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
