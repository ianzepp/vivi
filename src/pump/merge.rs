//! The merge paths: one unit branch at a time, and the batch path with
//! culprit isolation.
//!
//! Behaviour port of pump.py `failure_text` (:179), `reset_repo` (:625),
//! `merge_work` (:632), `batch_round` (:675) and `batch_work` (:749): reset
//! the integration clone, merge unit branches, format, run the checks, and
//! push — or report why it did not land.

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

/// A cleanly merged job plus the files its merge touched outside its scope.
struct Merged {
    job: MergeJob,
    outside: Vec<String>,
}

/// What a shared check decided about the batch.
enum SharedOutcome {
    /// Every shared check passed; keep going.
    Pass,
    /// The batch had one job: it is the culprit and is decided.
    Single(MergeResult),
    /// Several jobs: no culprit is known.
    Many,
}

/// Merge a batch of unit branches together, check the result once and push
/// once (pump.py `batch_round`). Returns the results for the decided jobs
/// and the undecided ones: `Some` lists the jobs a second round should retry
/// (empty when none remain), `None` when no culprit is known.
#[must_use]
pub fn batch_round(ctx: &Ctx, jobs: &[MergeJob]) -> (Vec<MergeResult>, Option<Vec<MergeJob>>) {
    let t0 = ctx.now();
    reset_repo(ctx);
    merge_main_in(ctx);
    let (mut results, merged) = merge_units(ctx, jobs);
    if merged.is_empty() {
        return (results, Some(Vec::new()));
    }
    let tag = style_tag(&merged);
    format_style_commit(ctx, &tag);
    match run_shared(ctx, &merged) {
        SharedOutcome::Pass => {}
        SharedOutcome::Single(result) => {
            results.push(result);
            return (results, Some(Vec::new()));
        }
        SharedOutcome::Many => return (results, None),
    }
    let culprits = run_proofs(ctx, &merged, &mut results);
    if !culprits.is_empty() {
        let rest: Vec<MergeJob> = merged
            .into_iter()
            .map(|m| m.job)
            .filter(|j| !culprits.contains(&j.uid))
            .collect();
        return (results, Some(rest));
    }
    results.extend(push_batch(ctx, merged, t0));
    (results, Some(Vec::new()))
}

/// Drive the batch rounds (pump.py `batch_work`): one round over all jobs, a
/// second over the survivors, and anything still undecided merged one at a
/// time with [`merge_work`].
#[must_use]
pub fn batch_work(ctx: &Ctx, jobs: &[MergeJob]) -> Vec<MergeResult> {
    let (mut out, first) = batch_round(ctx, jobs);
    let mut rest = first;
    if rest.as_ref().is_some_and(|p| !p.is_empty()) {
        let pending = rest.clone().unwrap_or_default();
        let (more, second) = batch_round(ctx, &pending);
        out.extend(more);
        rest = match second {
            None => Some(pending),
            decided => decided,
        };
        if rest.as_ref().is_some_and(Vec::is_empty) {
            return out;
        }
    }
    let leftover = match rest {
        None => jobs
            .iter()
            .filter(|j| !out.iter().any(|r| r.uid == j.uid))
            .cloned()
            .collect(),
        Some(list) => list,
    };
    for job in &leftover {
        out.push(merge_work(ctx, job));
    }
    out
}

/// The joined uids for the batch's style commit, cut to 150 characters.
fn style_tag(merged: &[Merged]) -> String {
    let uids: Vec<&str> = merged.iter().map(|m| m.job.uid.as_str()).collect();
    uids.join(",").chars().take(150).collect()
}

/// Merge main into the integration branch when it is not an ancestor yet
/// (batch path only). A conflict is aborted and logged; the batch continues.
fn merge_main_in(ctx: &Ctx) {
    let main = ctx.cfg.main.as_str();
    if main.is_empty() {
        return;
    }
    let remote = ctx.cfg.remote.as_str();
    ctx.git(&format!("fetch -q {remote} {main}"), 120);
    let reference = format!("{remote}/{main}");
    let cmd = format!("merge-base --is-ancestor {reference} HEAD");
    if ctx.git(&cmd, GIT_TIMEOUT).code == 0 {
        return;
    }
    let msg = shell_quote(&format!("merge {main} into {}", ctx.cfg.branch));
    let out = ctx.git(
        &format!("merge --no-ff --no-edit -m {msg} {reference}"),
        GIT_TIMEOUT,
    );
    if out.code == 0 {
        ctx.log(&format!("merged {main} into {}", ctx.cfg.branch));
        return;
    }
    ctx.git("merge --abort", GIT_TIMEOUT);
    let flat = tail(&out.text, 200).replace('\n', " | ");
    ctx.log(&format!(
        "{main} does not merge into {} cleanly: {flat}",
        ctx.cfg.branch
    ));
}

/// Fetch and merge every job; conflicts decide their job, clean merges
/// collect with their `outside` files.
fn merge_units(ctx: &Ctx, jobs: &[MergeJob]) -> (Vec<MergeResult>, Vec<Merged>) {
    let mut results = Vec::new();
    let mut merged = Vec::new();
    for job in jobs {
        if let Some(result) = merge_one(ctx, job) {
            results.push(result);
        } else {
            let outside = outside_files(ctx, job);
            merged.push(Merged {
                job: job.clone(),
                outside,
            });
        }
    }
    (results, merged)
}

/// Fetch and merge one job's branch. `None` means it merged cleanly.
fn merge_one(ctx: &Ctx, job: &MergeJob) -> Option<MergeResult> {
    let fetch = format!(
        "fetch {r} refs/heads/{b}:refs/remotes/{r}/{b}",
        r = ctx.cfg.remote,
        b = job.branch
    );
    let out = ctx.git(&fetch, 180);
    if out.code != 0 {
        return Some(failed(&job.uid, "fetch-failed", out.text));
    }
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
    ctx.git("merge --abort", GIT_TIMEOUT);
    let detail = format!(
        "The branch no longer merges into the integration branch. Conflicting files: {files}"
    );
    Some(failed(&job.uid, "merge-conflict", detail))
}

/// Run the shared checks once over the whole batch, each with a
/// `batch <n> shared rc=<n> <n>s: <cmd>` log line.
fn run_shared(ctx: &Ctx, merged: &[Merged]) -> SharedOutcome {
    for cmd in &ctx.cfg.merge.shared_checks {
        let t1 = ctx.now();
        let out = ctx.sh(cmd, Some(&ctx.repo), ctx.cfg.merge.proof_timeout);
        ctx.log(&format!(
            "batch {} shared rc={} {}s: {cmd}",
            merged.len(),
            out.code,
            ctx.now() - t1
        ));
        if out.code != 0 {
            return shared_failure(ctx, merged, cmd, out.code, &out.text);
        }
    }
    SharedOutcome::Pass
}

/// Decide a failed shared check: one merged job is the culprit; several
/// leave the batch undecided.
fn shared_failure(ctx: &Ctx, merged: &[Merged], cmd: &str, code: i32, text: &str) -> SharedOutcome {
    if merged.len() == 1 {
        reset_repo(ctx);
        let detail = proof_detail(code, cmd, text);
        return SharedOutcome::Single(failed(&merged[0].job.uid, "proof-failed", detail));
    }
    SharedOutcome::Many
}

/// Run each merged job's own proof (skipping `full` tools); a failing job
/// becomes a `proof-failed` result and joins the culprits.
fn run_proofs(ctx: &Ctx, merged: &[Merged], results: &mut Vec<MergeResult>) -> Vec<String> {
    let mut culprits = Vec::new();
    for m in merged {
        if m.job.tools == "full" {
            continue;
        }
        for cmd in &m.job.proof {
            let t1 = ctx.now();
            let out = ctx.sh(cmd, Some(&ctx.repo), ctx.cfg.merge.proof_timeout);
            let head: String = cmd.chars().take(90).collect();
            ctx.log(&format!(
                "proof {} rc={} {}s: {head}",
                m.job.uid,
                out.code,
                ctx.now() - t1
            ));
            if out.code != 0 {
                culprits.push(m.job.uid.clone());
                let detail = proof_detail(out.code, cmd, &out.text);
                results.push(failed(&m.job.uid, "proof-failed", detail));
                break;
            }
        }
    }
    culprits
}

/// The batch proof-failure detail: the command and what it printed.
fn proof_detail(code: i32, cmd: &str, out: &str) -> String {
    format!(
        "After merging into the integration branch, this command failed (exit {code}):\n{cmd}\n\nWhat failed:\n{}",
        failure_text(out)
    )
}

/// Discard stray changes, push the batch once, and shape one result per
/// merged job.
fn push_batch(ctx: &Ctx, merged: Vec<Merged>, t0: u64) -> Vec<MergeResult> {
    let dirty = ctx.git("status --porcelain", GIT_TIMEOUT).text;
    if !dirty.trim().is_empty() {
        ctx.git("checkout -q -- .", GIT_TIMEOUT);
    }
    let cmd = format!("push {} HEAD:refs/heads/{}", ctx.cfg.remote, ctx.cfg.branch);
    let out = ctx.git(&cmd, 180);
    if out.code != 0 {
        reset_repo(ctx);
        return merged
            .into_iter()
            .map(|m| push_failed(&m.job.uid, &out.text))
            .collect();
    }
    let tip = ctx
        .git("rev-parse HEAD", GIT_TIMEOUT)
        .text
        .trim()
        .to_string();
    let per = std::cmp::max(1, (ctx.now() - t0) / (merged.len() as u64));
    merged
        .into_iter()
        .map(|m| MergeResult {
            uid: m.job.uid,
            ok: Some(true),
            tip: Some(tip.clone()),
            outside: m.outside,
            secs: Some(per),
            why: None,
            detail: None,
        })
        .collect()
}

/// An undecided (`ok=null`) push-failure result: the last 200 chars of the
/// push output.
fn push_failed(uid: &str, out: &str) -> MergeResult {
    MergeResult {
        uid: uid.to_string(),
        ok: None,
        tip: None,
        outside: Vec::new(),
        secs: None,
        why: Some(String::from("push-failed")),
        detail: Some(tail(out, 200)),
    }
}
