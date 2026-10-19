//! The merge paths: one unit branch at a time, the batch path with culprit
//! isolation and quarantine, and the parallel merge lanes that run them.
//!
//! This file holds the single-unit path (`merge_work`) and the helpers every
//! path shares. The batch rounds live in `batch`, the publish routine that
//! pushes a lane's result in `publish`, the lane threads and the pull-model
//! [`Merger`] in `lane`, and the result writers in `apply`.

mod apply;
mod batch;
mod fetch;
mod lane;
mod publish;

pub use batch::{batch_round, batch_work};
pub(crate) use fetch::fetch_unit_branch;
pub use lane::Merger;
#[cfg(test)]
pub(crate) use lane::dispatch_results;

use std::time::UNIX_EPOCH;
use std::{fs, io};

use crate::pump::config::Generated;
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

/// Seconds a `.git/index.lock` must go untouched before a lane treats it as
/// stale. The age threshold is the practical proxy for the "no live git
/// process" probe: a live git process would have written the lock recently.
pub(crate) const STALE_LOCK_SECS: u64 = 120;

/// Clear a stale `.git/index.lock` in the checkout before a batch.
///
/// git refuses to touch the index while the lock exists, so a lane killed
/// mid-checkout wedges every later merge as a phantom conflict. There is no
/// process probing: the lock's mtime age stands in for "no live git process",
/// because a live git process would have written the lock recently. A lock
/// younger than [`STALE_LOCK_SECS`] may be held by a live git process (lane
/// 0's checkout is also used by the gate pass on the main thread), so it is
/// reported, never removed. An absent lock is `Ok(())`.
pub(crate) fn clear_stale_lock(ctx: &Ctx) -> Result<(), String> {
    let path = ctx.repo.join(".git/index.lock");
    let meta = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("index.lock: {err}")),
    };
    let modified = meta
        .modified()
        .map_err(|err| format!("index.lock mtime: {err}"))?;
    let written = modified
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let age = ctx.now().saturating_sub(written);
    if age < STALE_LOCK_SECS {
        return Err(format!(
            "index.lock exists and is only {age}s old; not cleared"
        ));
    }
    fs::remove_file(&path).map_err(|err| format!("remove index.lock: {err}"))?;
    ctx.log(&format!("cleared stale index.lock (age {age}s)"));
    Ok(())
}

/// Abort any merge in progress and hard-reset the integration clone to
/// `remote/branch`.
pub fn reset_repo(ctx: &Ctx) {
    if let Err(err) = clear_stale_lock(ctx) {
        ctx.log(&format!("lane reset: {err}"));
    }
    let branch = ctx.cfg.branch.as_str();
    let remote = ctx.cfg.remote.as_str();
    drop(ctx.git("merge --abort", GIT_TIMEOUT));
    drop(ctx.git(&format!("fetch {remote} {branch}"), 120));
    drop(ctx.git(&format!("checkout -q {branch}"), GIT_TIMEOUT));
    drop(ctx.git(&format!("reset -q --hard {remote}/{branch}"), GIT_TIMEOUT));
}

/// Clear a stale lock, reset the checkout to `remote/branch`, and prove the
/// reset actually reached the remote tip: `HEAD` must equal
/// `<remote>/<branch>`. A lane whose reset failed must not be handed units,
/// because every merge would then replay against a stale base and burn
/// attempts.
pub(crate) fn prepare_lane(ctx: &Ctx) -> Result<(), String> {
    clear_stale_lock(ctx)?;
    reset_repo(ctx);
    let head = ctx.git("rev-parse HEAD", GIT_TIMEOUT);
    if head.code != 0 {
        return Err(tail(&head.text, 300));
    }
    let remote_ref = format!("{}/{}", ctx.cfg.remote, ctx.cfg.branch);
    let tip = ctx.git(&format!("rev-parse {remote_ref}"), GIT_TIMEOUT);
    if tip.code != 0 {
        return Err(tail(&tip.text, 300));
    }
    let head = head.text.trim();
    let tip = tip.text.trim();
    if head != tip {
        return Err(format!(
            "HEAD is {head} but {remote_ref} is {tip} after reset"
        ));
    }
    Ok(())
}

/// Merge one unit branch into the integration branch, run the checks and
/// push. Runs on the merge thread and touches only
/// the repository, never the pump state.
#[must_use]
pub fn merge_work(ctx: &Ctx, job: &MergeJob) -> MergeResult {
    let t0 = ctx.now();
    reset_repo(ctx);
    let out = fetch_unit_branch(ctx, job);
    if out.code != 0 {
        merge_line(
            ctx,
            &job.uid,
            (1, 1),
            &format!("fetch failed: {}", flat(&out.text)),
        );
        return fetch_failure(&job.uid, &out.text);
    }
    if let Some(result) = already_integrated(ctx, job, t0) {
        merge_line(ctx, &job.uid, (1, 1), &integrated_text(&result));
        return result;
    }
    if let Some(result) = merge_branch(ctx, job) {
        merge_line(ctx, &job.uid, (1, 1), "conflict");
        return result;
    }
    merge_line(ctx, &job.uid, (1, 1), "ok");
    let outside = outside_files(ctx, job);
    format_style_commit(ctx, &job.uid);
    if let Some(result) = run_checks(ctx, job) {
        return result;
    }
    push(ctx, &job.uid, outside, t0)
}

/// One merge step line: `merge <uid> <k>/<n> <what>`. Also the lane's
/// current step.
fn merge_line(ctx: &Ctx, uid: &str, (k, n): (usize, usize), what: &str) {
    ctx.step(&format!("merge {uid} {k}/{n} {what}"));
}

/// What the integration check of a replayed branch decided, for the log.
fn integrated_text(result: &MergeResult) -> String {
    match &result.why {
        None => String::from("already integrated"),
        Some(_) => String::from("integration check failed"),
    }
}

/// The last 120 characters of `text` on one line.
fn flat(text: &str) -> String {
    tail(text, 120).replace('\n', " | ")
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
        quarantine: None,
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
        quarantine: None,
    }
}

/// The declared generated-file entries covering every file in `files`,
/// deduplicated by entry; `None` when any file has no covering entry.
pub(crate) fn generated_entries<'a>(ctx: &'a Ctx, files: &[String]) -> Option<Vec<&'a Generated>> {
    let mut entries: Vec<&Generated> = Vec::new();
    'files: for file in files {
        for entry in &ctx.cfg.merge.generated {
            if overlaps(&[file.clone()], &entry.paths) {
                if !entries.iter().any(|e| std::ptr::eq(*e, entry)) {
                    entries.push(entry);
                }
                continue 'files;
            }
        }
        return None;
    }
    Some(entries)
}

/// Regenerate the generated files from source of truth and stage them. No
/// reset inside: the caller owns recovery.
pub(crate) fn regenerate_generated(ctx: &Ctx, entries: &[&Generated]) -> Result<(), String> {
    for entry in entries {
        let out = ctx.sh(
            &entry.regenerate,
            Some(&ctx.repo),
            ctx.cfg.merge.proof_timeout,
        );
        if out.code != 0 {
            return Err(tail(&out.text, 300));
        }
        let paths = entry
            .paths
            .iter()
            .map(|p| shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ");
        let out = ctx.git(&format!("add -- {paths}"), GIT_TIMEOUT);
        if out.code != 0 {
            return Err(tail(&out.text, 300));
        }
    }
    Ok(())
}

/// Run a check, and when it fails with a declared drift signature, regenerate
/// the named generated entry and run the check once more. Returns the
/// decisive output: the rerun's when a retry happened, otherwise the first.
pub(crate) fn check_with_drift_retry(
    ctx: &Ctx,
    uid: &str,
    cmd: &str,
) -> crate::pump::host::CmdOutput {
    let first = ctx.sh(cmd, Some(&ctx.repo), ctx.cfg.merge.proof_timeout);
    if first.code == 0 {
        return first;
    }
    let drifted = ctx.cfg.merge.generated.iter().find(|entry| {
        entry
            .drift
            .iter()
            .any(|sig| first.text.contains(sig.as_str()))
    });
    let Some(entry) = drifted else {
        return first;
    };
    let Some(head) = entry.paths.first() else {
        return first;
    };
    if let Err(text) = regenerate_generated(ctx, &[entry]) {
        ctx.log(&format!("generated regenerate failed {uid}: {text}"));
        return first;
    }
    let msg = shell_quote(&format!("regen: {head} after {uid}"));
    let commit = format!("{} commit -q -m {msg}", identity_args(ctx));
    if ctx.git(&commit, GIT_TIMEOUT).code != 0 {
        return first;
    }
    ctx.log(&format!(
        "drift {uid}: regenerated {}; retrying once",
        entry.paths.join(" ")
    ));
    ctx.sh(cmd, Some(&ctx.repo), ctx.cfg.merge.proof_timeout)
}

/// Resolve a merge conflict touching only generated files: take the
/// integration side, regenerate the files, and conclude the merge commit.
/// Any failing step resets the repo; success logs the resolution.
pub(crate) fn resolve_generated(ctx: &Ctx, uid: &str, files: &[String]) -> bool {
    let Some(entries) = generated_entries(ctx, files) else {
        return false;
    };
    for file in files {
        let out = ctx.git(&format!("checkout --ours -- {file}"), GIT_TIMEOUT);
        if out.code != 0 {
            reset_repo(ctx);
            return false;
        }
    }
    if let Err(text) = regenerate_generated(ctx, &entries) {
        reset_repo(ctx);
        ctx.log(&format!("generated regenerate failed {uid}: {text}"));
        return false;
    }
    let commit = format!("{} commit --no-edit", identity_args(ctx));
    let out = ctx.git(&commit, GIT_TIMEOUT);
    if out.code != 0 {
        reset_repo(ctx);
        return false;
    }
    ctx.log(&format!(
        "resolved generated conflicts {uid}: {}",
        files.join(" ")
    ));
    true
}

/// Merge the unit branch; on conflict, resolve conflicts on generated files
/// by regenerating them, and otherwise reset and report the conflicting
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
    let raw = ctx
        .git("diff --name-only --diff-filter=U", GIT_TIMEOUT)
        .text;
    let files: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect();
    if !files.is_empty() && resolve_generated(ctx, &job.uid, &files) {
        return None;
    }
    reset_repo(ctx);
    let detail = format!(
        "The branch no longer merges into the integration branch. Conflicting files: {raw}"
    );
    Some(failed(&job.uid, "merge-conflict", detail))
}

/// Files the merge touched outside the unit's write scope. Generated
/// paths never count: a regenerated file committed by the merge lane is
/// rebuilt from source of truth, not written by hand.
fn outside_files(ctx: &Ctx, job: &MergeJob) -> Vec<String> {
    let generated = ctx.cfg.merge.generated_paths();
    let changed = ctx.git("diff --name-only HEAD~1 HEAD", GIT_TIMEOUT).text;
    changed
        .split_whitespace()
        .filter(|f| !overlaps(&[f.to_string()], &job.write_scope))
        .filter(|f| !overlaps(&[f.to_string()], &generated))
        .map(str::to_string)
        .collect()
}

/// Run the configured formatter and commit any change it made as a style
/// commit, before the checks run. An empty `format` skips the whole step.
fn format_style_commit(ctx: &Ctx, uid: &str) {
    if ctx.cfg.merge.format.is_empty() {
        return;
    }
    ctx.step(&format!("format: {}", ctx.cfg.merge.format));
    let started = ctx.now();
    drop(ctx.sh(&ctx.cfg.merge.format, Some(&ctx.repo), 180));
    ctx.log(&format!(
        "format done {}s",
        ctx.now().saturating_sub(started)
    ));
    let dirty = ctx.git("status --porcelain", GIT_TIMEOUT).text;
    if !dirty.trim().is_empty() {
        ctx.step("style commit");
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
    let shared = ctx.cfg.merge.shared_checks.len();
    for (i, cmd) in checks.iter().enumerate() {
        let head: String = cmd.chars().take(90).collect();
        if i < shared {
            ctx.step(&format!("check {}/{shared} start: {head}", i + 1));
        } else {
            ctx.step(&format!("proof {} start: {head}", job.uid));
        }
        let t1 = ctx.now();
        let out = check_with_drift_retry(ctx, &job.uid, cmd);
        ctx.log(&format!(
            "proof {} rc={} {}s: {head}",
            job.uid,
            out.code,
            ctx.now().saturating_sub(t1)
        ));
        if out.code != 0 {
            reset_repo(ctx);
            let last = failure_text(&out.text);
            if crate::pump::envfail::missing_tool(out.code, &out.text) {
                let detail = format!(
                    "After merging into the integration branch, this command could not run (exit {}):\n{cmd}\n\nLast output:\n{last}",
                    out.code
                );
                return Some(undecided_result(&job.uid, "proof-missing-tool", &detail));
            }
            if i >= shared && crate::pump::proof::usage_failure(&out.text) {
                let detail = format!(
                    "The unit's proof command failed as a usage error before anything ran (exit {}):\n{cmd}\n\nLast output:\n{last}",
                    out.code
                );
                return Some(failed(&job.uid, "proof-usage", detail));
            }
            let detail = format!(
                "After merging into the integration branch, this command failed (exit {}):\n{cmd}\n\nLast output:\n{last}",
                out.code
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
            quarantine: None,
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
        quarantine: None,
    }
}
