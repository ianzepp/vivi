//! The single-job merge path.
//!
//! Behaviour port of pump.py `failure_text` (:179), `reset_repo` (:625) and
//! `merge_work` (:632): reset the integration clone, merge one unit branch,
//! format, run the checks, and push — or report why it did not land.

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::plan::overlaps;
use crate::pump::state::{MergeJob, MergeResult};

/// Default git timeout in seconds, as pump.py's `git()` helper.
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

/// Abort any merge in progress and hard-reset the integration clone to
/// `remote/branch` (pump.py `reset_repo`).
pub fn reset_repo(ctx: &Ctx) {
    let branch = ctx.cfg.branch.as_str();
    let remote = ctx.cfg.remote.as_str();
    ctx.git("merge --abort", GIT_TIMEOUT);
    ctx.git(&format!("fetch {remote} {branch}"), 120);
    ctx.git(&format!("checkout -q {branch}"), GIT_TIMEOUT);
    ctx.git(&format!("reset -q --hard {remote}/{branch}"), GIT_TIMEOUT);
}

/// Merge one unit branch into the integration branch, run the checks and
/// push (pump.py `merge_work`). Runs on the merge thread and touches only
/// the repository, never the pump state.
#[must_use]
pub fn merge_work(ctx: &Ctx, job: &MergeJob) -> MergeResult {
    let t0 = ctx.now();
    reset_repo(ctx);
    let fetch = format!(
        "fetch {r} refs/heads/{b}:refs/remotes/{r}/{b}",
        r = ctx.cfg.remote,
        b = job.branch
    );
    let out = ctx.git(&fetch, 180);
    if out.code != 0 {
        return failed(&job.uid, "fetch-failed", out.text);
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
        "merge --no-ff --no-edit -m {msg} {}/{}",
        ctx.cfg.remote, job.branch
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
    ctx.sh(&ctx.cfg.merge.format, Some(&ctx.repo), 180);
    let dirty = ctx.git("status --porcelain", GIT_TIMEOUT).text;
    if !dirty.trim().is_empty() {
        let msg = shell_quote(&format!("style: cargo fmt after {uid}"));
        ctx.git(&format!("commit -q -am {msg}"), GIT_TIMEOUT);
    }
}

/// Shared checks, then the unit's own proof (skipped for `full` tools). The
/// first failure resets the repo and decides the job.
fn run_checks(ctx: &Ctx, job: &MergeJob) -> Option<MergeResult> {
    let mut checks = ctx.cfg.merge.shared_checks.clone();
    if job.tools != "full" {
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
            ctx.now() - t1
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

/// Discard any dirt the checks left, push the integration branch, and shape
/// the final result.
fn push(ctx: &Ctx, uid: &str, outside: Vec<String>, t0: u64) -> MergeResult {
    let dirty = ctx.git("status --porcelain", GIT_TIMEOUT).text;
    if !dirty.trim().is_empty() {
        ctx.git("checkout -q -- .", GIT_TIMEOUT);
    }
    let cmd = format!("push {} HEAD:refs/heads/{}", ctx.cfg.remote, ctx.cfg.branch);
    let out = ctx.git(&cmd, 180);
    if out.code != 0 {
        reset_repo(ctx);
        return MergeResult {
            uid: uid.to_string(),
            ok: None,
            tip: None,
            outside: Vec::new(),
            secs: None,
            why: Some(String::from("push-failed")),
            detail: Some(tail(&out.text, 200)),
        };
    }
    let tip = ctx
        .git("rev-parse HEAD", GIT_TIMEOUT)
        .text
        .trim()
        .to_string();
    MergeResult {
        uid: uid.to_string(),
        ok: Some(true),
        tip: Some(tip),
        outside,
        secs: Some(ctx.now() - t0),
        why: None,
        detail: None,
    }
}
