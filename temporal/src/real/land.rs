//! The Lander's three steps, each a wrapper over a `lab/lander` command or
//! one fenced git push.
//!
//! - `compose` (lane host): refresh the lane's bare cache, then run
//!   `compose --no-fetch --no-push --base <main>` with `LANDER_CACHE` set to
//!   that cache. The runtime fetches and pushes itself, against the target's
//!   own remote, so compose's built-in remote is never used. The candidate
//!   is pushed to `refs/vivi/candidates/<id>` for the gate hosts.
//! - `gate` (gate host): `gate --sha <candidate> --base <main> --tree <tree>`.
//! - `publish` (lane host): a fast-forward of main to the candidate with
//!   `--force-with-lease=<main>:<base>`; a moved main is `Stale`.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use temporalio_sdk::activities::ActivityError;

use crate::config::{Target, expand};
use crate::real::cmd::{Cmd, run, tail};
use crate::real::git::{Pushed, git, git_ok, infra, push_fenced};
use crate::types::{Candidate, ComposeInput, ComposeResult, Excluded, GateResult, PublishResult};

/// Refresh the cache (cloning it bare on first use) and return main's sha.
async fn refresh_cache(cache: &Path, target: &Target) -> Result<String, ActivityError> {
    if !cache.exists() {
        let parent = cache.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(parent).map_err(|e| infra(format!("{}: {e}", parent.display())))?;
        let dest = cache.display().to_string();
        git_ok(parent, &["clone", "-q", "--bare", &target.remote, &dest]).await?;
    }
    let mut specs = vec![format!(
        "+refs/heads/{0}*:refs/heads/{0}*",
        target.branch_prefix
    )];
    if !target.main.starts_with(&target.branch_prefix) {
        specs.push(format!("+refs/heads/{0}:refs/heads/{0}", target.main));
    }
    let mut args = vec!["fetch", "-q", "--prune", target.remote.as_str()];
    args.extend(specs.iter().map(String::as_str));
    git_ok(cache, &args).await?;
    git_ok(
        cache,
        &["rev-parse", &format!("refs/heads/{}", target.main)],
    )
    .await
}

pub async fn compose(target: &Target, input: ComposeInput) -> Result<ComposeResult, ActivityError> {
    let cache = expand(&target.lander.cache);
    let base = refresh_cache(&cache, target).await?;
    let mut cmd = Cmd::new(
        &target.lander.compose,
        &["--no-fetch", "--no-push", "--base", &base],
        Duration::from_secs(600),
    )
    .env("LANDER_CACHE", &cache.display().to_string());
    cmd.args
        .extend(input.units.iter().map(|u| u.branch.clone()));
    let out = run(&cmd).await.map_err(|e| infra(e.to_string()))?;
    if !out.ok() {
        return Err(infra(format!("compose: {}", out.tail(800))));
    }
    let report: Value = serde_json::from_str(&out.stdout)
        .map_err(|e| infra(format!("compose printed no JSON ({e}): {}", out.tail(400))))?;
    let mut result = read_compose(&report, &input, &base)
        .ok_or_else(|| infra(format!("compose JSON not understood: {}", out.tail(400))))?;
    if let Some(candidate) = &result.candidate {
        // One candidate is alive per target at a time (the Lander works its
        // batches and halves in turn), so any left on the lane is spent.
        drop_spent_candidates(&cache, target, &input.target).await;
        let refname = format!("{}:refs/vivi/candidates/{}", candidate.sha, candidate.id);
        git_ok(&cache, &["push", "-q", &target.remote, &refname]).await?;
    }
    result.already.sort_by(|a, b| a.slice.cmp(&b.slice));
    Ok(result)
}

/// Map compose's JSON report back onto the land requests.
pub fn read_compose(report: &Value, input: &ComposeInput, base: &str) -> Option<ComposeResult> {
    let unit = |branch: &str| input.units.iter().find(|u| u.branch == branch).cloned();
    let mut included = Vec::new();
    for entry in report.get("included")?.as_array()? {
        included.extend(unit(entry.get("branch")?.as_str()?));
    }
    let mut excluded = Vec::new();
    let mut already = Vec::new();
    for entry in report.get("excluded")?.as_array()? {
        let Some(u) = unit(entry.get("branch")?.as_str()?) else {
            continue;
        };
        match entry.get("reason")?.as_str()? {
            "already-merged" | "no-op" => already.push(u),
            reason => excluded.push(Excluded {
                unit: u,
                reason: exclusion_text(reason, entry.get("files")),
            }),
        }
    }
    let candidate = match report.get("candidate")? {
        Value::String(sha) if !included.is_empty() => Some(Candidate {
            // `<target>/<sha>`: each target's candidate refs sit in their own
            // folder, so one target's cleanup never touches another's.
            id: format!("{}/{}", input.target, sha.get(..12).unwrap_or(sha)),
            target: input.target.clone(),
            base: base.to_string(),
            sha: sha.clone(),
            units: included,
        }),
        _ => None,
    };
    Some(ComposeResult {
        candidate,
        excluded,
        already,
    })
}

fn exclusion_text(reason: &str, files: Option<&Value>) -> String {
    let files: Vec<&str> = files
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match (reason, files.is_empty()) {
        ("conflict", false) => format!("conflicts with main in {}", files.join(", ")),
        ("missing", _) => "the branch is missing on the lane".to_string(),
        (other, _) => other.to_string(),
    }
}

pub async fn gate(target: &Target, candidate: &Candidate) -> Result<GateResult, ActivityError> {
    let tree = expand(&target.verify_tree).display().to_string();
    let timeout = target.lander.gate_timeout_s.to_string();
    let args = [
        "--sha",
        &candidate.sha,
        "--base",
        &candidate.base,
        "--tree",
        &tree,
        "--timeout",
        &timeout,
    ];
    let limit = Duration::from_secs(target.lander.gate_timeout_s.saturating_mul(2) + 120);
    let out = run(&Cmd::new(&target.lander.gate, &args, limit))
        .await
        .map_err(|e| infra(e.to_string()))?;
    match out.rc {
        Some(0) => {
            let report: Value = serde_json::from_str(out.stdout.trim())
                .map_err(|e| infra(format!("gate printed no JSON ({e}): {}", out.tail(400))))?;
            read_gate(&report).map_err(infra)
        }
        Some(3) => Err(infra(format!("gate timed out: {}", out.tail(400)))),
        _ => Err(infra(format!("gate could not run: {}", out.tail(800)))),
    }
}

/// The gate's verdict. A broken base is not the candidate's fault, so it
/// is an error (retried), never a red that would eject good units.
pub fn read_gate(report: &Value) -> Result<GateResult, String> {
    let status = report.get("status").and_then(Value::as_str).unwrap_or("");
    if status.starts_with("baseline_") || status == "error" || status == "timeout" {
        return Err(format!("gate status {status}: {report}"));
    }
    match report.get("pass").and_then(Value::as_bool) {
        Some(true) => Ok(GateResult::Pass),
        Some(false) => Ok(GateResult::Fail {
            text: gate_failure_text(report, status),
        }),
        None => Err(format!("gate gave no pass/fail: {report}")),
    }
}

fn gate_failure_text(report: &Value, status: &str) -> String {
    let list = |key: &str| -> Vec<String> {
        report
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .take(20)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let sha = report.get("sha").and_then(Value::as_str).unwrap_or("?");
    let mut text = format!("gate red on candidate {sha} ({status})");
    let failures = list("new_failures");
    if !failures.is_empty() {
        text.push_str(&format!("; new failures: {}", failures.join(", ")));
    }
    let errors = list("build_errors");
    if !errors.is_empty() {
        text.push_str(&format!("; build errors: {}", errors.join(" | ")));
    }
    tail(&text, 3_000)
}

pub async fn publish(
    target: &Target,
    candidate: &Candidate,
) -> Result<PublishResult, ActivityError> {
    let cache = expand(&target.lander.cache);
    let args = [
        "merge-base",
        "--is-ancestor",
        &candidate.base,
        &candidate.sha,
    ];
    if git_ok(&cache, &args).await.is_err() {
        return Err(infra(format!(
            "candidate {} does not descend from base {}",
            candidate.sha, candidate.base
        )));
    }
    let pushed = push_fenced(
        &cache,
        &target.remote,
        &candidate.sha,
        &target.main,
        Some(&candidate.base),
    );
    match pushed.await? {
        Pushed::Accepted => {
            drop_landed(&cache, target, candidate).await;
            Ok(PublishResult::Published {
                sha: candidate.sha.clone(),
            })
        }
        Pushed::Fenced(text) => Ok(PublishResult::Stale {
            text: format!("{} moved off {}: {text}", target.main, candidate.base),
        }),
    }
}

/// The refspecs that delete `refs/vivi/candidates/<target>/*` on the lane.
pub fn spent_candidate_deletes(ls_remote: &str, target: &str) -> Vec<String> {
    let prefix = format!("refs/vivi/candidates/{target}/");
    ls_remote
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(_, name)| name.trim())
        .filter(|name| name.starts_with(&prefix))
        .map(|name| format!(":{name}"))
        .collect()
}

/// Delete this target's leftover candidate refs. Best effort: a failure
/// leaves them for the next compose.
async fn drop_spent_candidates(cache: &Path, target: &Target, name: &str) {
    let pattern = format!("refs/vivi/candidates/{name}/*");
    let Ok(listed) = git_ok(cache, &["ls-remote", &target.remote, &pattern]).await else {
        return;
    };
    let deletes = spent_candidate_deletes(&listed, name);
    if deletes.is_empty() {
        return;
    }
    let mut args = vec!["push", "-q", target.remote.as_str()];
    args.extend(deletes.iter().map(String::as_str));
    let _ = git(cache, &args).await;
}

/// After a publish: delete the landed slice branches, each only if it still
/// points at the head that landed, and the candidate's ref. Best effort.
async fn drop_landed(cache: &Path, target: &Target, candidate: &Candidate) {
    let mut args = vec!["push".to_string(), "-q".to_string(), "--atomic".to_string()];
    let mut deletes = Vec::new();
    for unit in &candidate.units {
        if unit.branch.is_empty() || unit.head.is_empty() {
            continue;
        }
        let refname = format!("refs/heads/{}", unit.branch);
        args.push(format!("--force-with-lease={refname}:{}", unit.head));
        deletes.push(format!(":{refname}"));
    }
    if !deletes.is_empty() {
        args.push(target.remote.clone());
        args.extend(deletes);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let _ = git(cache, &args).await;
    }
    let candidate_ref = format!(":refs/vivi/candidates/{}", candidate.id);
    let _ = git(cache, &["push", "-q", &target.remote, &candidate_ref]).await;
}

#[cfg(test)]
#[path = "land_test.rs"]
mod tests;
