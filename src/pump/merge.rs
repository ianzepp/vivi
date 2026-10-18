//! The merge paths: one unit branch at a time, the batch path with culprit
//! isolation, and the parallel merge lanes that run them.
//!
//! This file holds the single-unit path (`merge_work`) and the helpers every
//! path shares. The batch rounds live in `batch`, the publish routine that
//! pushes a lane's result in `publish`, the lane threads and the pull-model
//! [`Merger`] in `lane`, and the result writers in `apply`.

mod apply;
mod batch;
mod lane;
mod publish;

pub use batch::{batch_round, batch_work};
pub use lane::Merger;

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::plan::overlaps;
use crate::pump::state::{MergeJob, MergeResult};

use publish::publish;

/// Default git timeout in seconds, for git commands.
const GIT_TIMEOUT: u64 = 300;

/// Distill a raw command output down to the lines that explain a failure:
/// `panicked at`, `---- ` headers, `error[`/`error:` lines, `assertion`s,
/// `failures:` headers and `FAILED` markers, each with its next three lines.
/// The kept lines are de-duplicated in order; the result is their first 1400
/// characters, a `...` marker, and the last 500 characters of the whole
/// output. With nothing kept, the last 1800 characters of the output.
#[must_use]
pub fn failure_text(out: &str) -> String {
    let lines: Vec<&str> = out.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if explains(line) {
            let end = (i + 4).min(lines.len());
            kept.extend_from_slice(&lines[i..end]);
        }
    }
    let mut uniq: Vec<&str> = Vec::new();
    for line in kept {
        if !uniq.contains(&line) {
            uniq.push(line);
        }
    }
    if uniq.is_empty() {
        return tail(out, 1800);
    }
    let head: String = uniq.join("\n").chars().take(1400).collect();
    format!("{head}\n...\n{}", tail(out, 500))
}

/// True when `line` is one of the failure markers `failure_text` keeps.
fn explains(line: &str) -> bool {
    line.contains("panicked at")
        || line.starts_with("---- ")
        || line.starts_with("error[")
        || line.starts_with("error:")
        || line.contains("assertion")
        || line.starts_with("failures:")
        || line.contains("FAILED")
}

/// The last `n` characters of `out`, cut on character boundaries.
fn tail(out: &str, n: usize) -> String {
    let len = out.chars().count();
    out.chars().skip(len.saturating_sub(n)).collect()
}

/// The `git -c` options that give a commit-making command the configured
/// identity: `-c user.name=<name> -c user.email=<email>`, shell-quoted. Used
/// as `ctx.git(&format!("{} merge ...", identity_args(ctx)), ..)`.
#[must_use]
pub fn identity_args(ctx: &Ctx) -> String {
    format!(
        "-c {} -c {}",
        shell_quote(&format!("user.name={}", ctx.cfg.committer_name)),
        shell_quote(&format!("user.email={}", ctx.cfg.committer_email)),
    )
}

/// Abort any merge in progress and hard-reset the integration clone to
/// `remote/branch`.
pub fn reset_repo(ctx: &Ctx) {
    let branch = ctx.cfg.branch.as_str();
    let remote = ctx.cfg.remote.as_str();
    drop(ctx.git("merge --abort", GIT_TIMEOUT));
    drop(ctx.git(&format!("fetch {remote} {branch}"), 120));
    drop(ctx.git(&format!("checkout -q {branch}"), GIT_TIMEOUT));
    drop(ctx.git(&format!("reset -q --hard {remote}/{branch}"), GIT_TIMEOUT));
}

/// Merge one unit branch into the integration branch, run the checks and
/// push. Runs on the merge thread and touches only
/// the repository, never the pump state.
#[must_use]
pub fn merge_work(ctx: &Ctx, job: &MergeJob) -> MergeResult {
    let t0 = ctx.now();
    reset_repo(ctx);
    let local = if ctx.cfg.targets.is_empty() {
        crate::pump::host::CmdOutput {
            code: 1,
            text: String::new(),
        }
    } else {
        ctx.git(
            &format!(
                "show-ref --verify --quiet refs/heads/{}",
                shell_quote(&job.branch)
            ),
            30,
        )
    };
    let fetch = if local.code == 0 {
        format!(
            "update-ref refs/remotes/{}/{} refs/heads/{}",
            ctx.cfg.remote,
            shell_quote(&job.branch),
            shell_quote(&job.branch)
        )
    } else {
        format!(
            "fetch {r} refs/heads/{b}:refs/remotes/{r}/{b}",
            r = ctx.cfg.remote,
            b = shell_quote(&job.branch)
        )
    };
    let out = ctx.git(&fetch, 180);
    if out.code != 0 {
        return fetch_failure(&job.uid, &out.text);
    }
    if let Some(result) = already_integrated(ctx, job, t0) {
        return result;
    }
    if let Some(result) = merge_branch(ctx, job) {
        return result;
    }
    let outside = outside_files(ctx, job);
    format_style_commit(ctx, &job.uid);
    if let Some(result) = run_checks(ctx, job) {
        return result;
    }
    push(ctx, &job.uid, outside, t0)
}

/// A replayed branch already reachable from the fetched integration tip
/// needs only its receipt restored. Never rerun checks or scope attribution.
fn already_integrated(ctx: &Ctx, job: &MergeJob, t0: u64) -> Option<MergeResult> {
    let integration = format!("{}/{}", ctx.cfg.remote, ctx.cfg.branch);
    let branch = format!("{}/{}", ctx.cfg.remote, job.branch);
    let out = ctx.git(
        &format!("merge-base --is-ancestor {branch} {integration}"),
        GIT_TIMEOUT,
    );
    match out.code {
        0 => {}
        1 => return None,
        _ => {
            return Some(undecided_result(
                &job.uid,
                "integration-check-failed",
                &out.text,
            ));
        }
    }
    let tip = ctx.git(&format!("rev-parse {integration}"), GIT_TIMEOUT);
    if tip.code != 0 || tip.text.trim().is_empty() {
        return Some(undecided_result(
            &job.uid,
            "integration-tip-failed",
            &tip.text,
        ));
    }
    Some(MergeResult {
        uid: job.uid.clone(),
        ok: Some(true),
        tip: Some(tip.text.trim().to_string()),
        outside: Vec::new(),
        secs: Some(ctx.now().saturating_sub(t0)),
        why: None,
        detail: None,
    })
}

/// The result of a failed fetch of a unit branch. A branch that does not
/// exist on the remote (git's "couldn't find remote ref") will never appear
/// by retrying, so it is a decided `branch-missing` failure; any other fetch
/// error (network, auth, ...) stays undecided and is retried.
fn fetch_failure(uid: &str, text: &str) -> MergeResult {
    if text.contains("couldn't find remote ref") {
        return failed(uid, "branch-missing", text.to_string());
    }
    undecided_result(uid, "fetch-failed", text)
}

/// A decided failure result (`ok=false`) with `why` and `detail`.
fn failed(uid: &str, why: &str, detail: String) -> MergeResult {
    MergeResult {
        uid: uid.to_string(),
        ok: Some(false),
        tip: None,
        outside: Vec::new(),
        secs: None,
        why: Some(why.to_string()),
        detail: Some(detail),
    }
}

/// Merge the unit branch; on conflict, reset and report the conflicting
/// files. `None` means the merge landed.
fn merge_branch(ctx: &Ctx, job: &MergeJob) -> Option<MergeResult> {
    let title: String = job.title.chars().take(80).collect();
    let msg = shell_quote(&format!("merge {}: {} (need {})", job.uid, title, job.need));
    let cmd = format!(
        "{} merge --no-ff --no-edit -m {msg} {}/{}",
        identity_args(ctx),
        ctx.cfg.remote,
        job.branch
    );
    let out = ctx.git(&cmd, GIT_TIMEOUT);
    if out.code == 0 {
        return None;
    }
    let files = ctx
        .git("diff --name-only --diff-filter=U", GIT_TIMEOUT)
        .text;
    reset_repo(ctx);
    let detail = format!(
        "The branch no longer merges into the integration branch. Conflicting files: {files}"
    );
    Some(failed(&job.uid, "merge-conflict", detail))
}

/// Files the merge touched outside the unit's write scope.
fn outside_files(ctx: &Ctx, job: &MergeJob) -> Vec<String> {
    let changed = ctx.git("diff --name-only HEAD~1 HEAD", GIT_TIMEOUT).text;
    changed
        .split_whitespace()
        .filter(|f| !overlaps(&[f.to_string()], &job.write_scope))
        .map(str::to_string)
        .collect()
}

/// Run the configured formatter and commit any change it made as a style
/// commit, before the checks run. An empty `format` skips the whole step.
fn format_style_commit(ctx: &Ctx, uid: &str) {
    if ctx.cfg.merge.format.is_empty() {
        return;
    }
    drop(ctx.sh(&ctx.cfg.merge.format, Some(&ctx.repo), 180));
    let dirty = ctx.git("status --porcelain", GIT_TIMEOUT).text;
    if !dirty.trim().is_empty() {
        let msg = shell_quote(&format!("style: format after {uid}"));
        let commit = format!("{} commit -q -am {msg}", identity_args(ctx));
        drop(ctx.git(&commit, GIT_TIMEOUT));
    }
}

/// Whether the kind a `tools` value selects skips its units' own proof.
fn skips_proof(ctx: &Ctx, tools: &str) -> bool {
    ctx.cfg.kind_of(tools).is_some_and(|kind| kind.skip_proof)
}

/// Shared checks, then the unit's own proof (skipped for a `skip_proof` kind). The
/// first failure resets the repo and decides the job.
fn run_checks(ctx: &Ctx, job: &MergeJob) -> Option<MergeResult> {
    let mut checks = ctx.cfg.merge.shared_checks.clone();
    if !skips_proof(ctx, &job.tools) {
        checks.extend(job.proof.iter().cloned());
    }
    for cmd in &checks {
        let t1 = ctx.now();
        let out = ctx.sh(cmd, Some(&ctx.repo), ctx.cfg.merge.proof_timeout);
        let head: String = cmd.chars().take(90).collect();
        ctx.log(&format!(
            "proof {} rc={} {}s: {head}",
            job.uid,
            out.code,
            ctx.now().saturating_sub(t1)
        ));
        if out.code != 0 {
            reset_repo(ctx);
            let detail = format!(
                "After merging into the integration branch, this command failed (exit {}):\n{cmd}\n\nLast output:\n{}",
                out.code,
                failure_text(&out.text)
            );
            return Some(failed(&job.uid, "proof-failed", detail));
        }
    }
    None
}

/// Publish the integration branch and shape the final result. A publish
/// that did not land is undecided: the unit returns to `delivered`.
fn push(ctx: &Ctx, uid: &str, outside: Vec<String>, t0: u64) -> MergeResult {
    match publish(ctx) {
        Ok(tip) => MergeResult {
            uid: uid.to_string(),
            ok: Some(true),
            tip: Some(tip),
            outside,
            secs: Some(ctx.now().saturating_sub(t0)),
            why: None,
            detail: None,
        },
        Err(failure) => undecided_result(uid, failure.why, &failure.detail),
    }
}

/// An `ok=None` result: the unit returns to `delivered` and is retried by a
/// later feed.
fn undecided_result(uid: &str, why: &str, detail: &str) -> MergeResult {
    MergeResult {
        uid: uid.to_string(),
        ok: None,
        tip: None,
        outside: Vec::new(),
        secs: None,
        why: Some(why.to_string()),
        detail: Some(detail.to_string()),
    }
}
