//! The merge paths: one unit branch at a time, and the batch path with
//! culprit isolation.
//!
//! `failure_text`, `reset_repo`, `merge_work`, `batch_round` and
//! `batch_work`: reset
//! the integration clone, merge unit branches, format, run the checks, and
//! push — or report why it did not land.

use std::any::Any;
use std::fs;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::pump::collect;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::plan::overlaps;
use crate::pump::ready::{dependency_holds, note_hold, priority_needs, ranked, satisfied};
use crate::pump::remote;
use crate::pump::state::{MergeJob, MergeResult, State, Status};

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

/// Discard any dirt the checks left, push the integration branch, and shape
/// the final result.
fn push(ctx: &Ctx, uid: &str, outside: Vec<String>, t0: u64) -> MergeResult {
    let dirty = ctx.git("status --porcelain", GIT_TIMEOUT).text;
    if !dirty.trim().is_empty() {
        drop(ctx.git("checkout -q -- .", GIT_TIMEOUT));
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
        secs: Some(ctx.now().saturating_sub(t0)),
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
/// once. Returns the results for the decided jobs
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

/// Drive the batch rounds: one round over all jobs, a
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
    drop(ctx.git(&format!("fetch -q {remote} {main}"), 120));
    let reference = format!("{remote}/{main}");
    let cmd = format!("merge-base --is-ancestor {reference} HEAD");
    if ctx.git(&cmd, GIT_TIMEOUT).code == 0 {
        return;
    }
    let msg = shell_quote(&format!("merge {main} into {}", ctx.cfg.branch));
    let out = ctx.git(
        &format!(
            "{} merge --no-ff --no-edit -m {msg} {reference}",
            identity_args(ctx)
        ),
        GIT_TIMEOUT,
    );
    if out.code == 0 {
        ctx.log(&format!("merged {main} into {}", ctx.cfg.branch));
        return;
    }
    drop(ctx.git("merge --abort", GIT_TIMEOUT));
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
        return Some(fetch_failure(&job.uid, &out.text));
    }
    if let Some(result) = already_integrated(ctx, job, ctx.now()) {
        return Some(result);
    }
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
    drop(ctx.git("merge --abort", GIT_TIMEOUT));
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
            ctx.now().saturating_sub(t1)
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

/// Whether the kind a `tools` value selects skips its units' own proof.
fn skips_proof(ctx: &Ctx, tools: &str) -> bool {
    ctx.cfg.kind_of(tools).is_some_and(|kind| kind.skip_proof)
}

/// Run each merged job's own proof (skipping `skip_proof` kinds); a failing job
/// becomes a `proof-failed` result and joins the culprits.
fn run_proofs(ctx: &Ctx, merged: &[Merged], results: &mut Vec<MergeResult>) -> Vec<String> {
    let mut culprits = Vec::new();
    for m in merged {
        if skips_proof(ctx, &m.job.tools) {
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
                ctx.now().saturating_sub(t1)
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
        drop(ctx.git("checkout -q -- .", GIT_TIMEOUT));
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
    let per = std::cmp::max(1, (ctx.now().saturating_sub(t0)) / (merged.len() as u64));
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

// ------------------------------------------------------------ the merge thread

/// The merge worker: one background thread that owns the integration clone,
/// fed by [`Merger::feed`] and drained by [`Merger::apply`].
///
/// The merge queues and
/// the merge thread: the thread blocks on one job, drains up
/// to `merge.batch` more, offers the batch to the remote merger and falls
/// back to [`batch_work`] or [`merge_work`]. It never touches the state;
/// every effect on units flows back through the results channel.
pub struct Merger {
    to_merge: Sender<MergeJob>,
    pub(crate) results: Receiver<MergeResult>,
}

impl Merger {
    /// Start the merge thread for `ctx`. It runs until this `Merger` is
    /// dropped: dropping closes the job channel, which ends the loop.
    #[must_use]
    pub fn start(ctx: &Ctx) -> Merger {
        let (to_merge, jobs) = channel();
        let (out, results) = channel();
        let worker = ctx.clone();
        let spawned = thread::Builder::new()
            .name(String::from("pump-merge"))
            .spawn(move || merge_loop(&worker, &jobs, &out));
        if spawned.is_err() {
            ctx.log("merge thread did not start; units stay delivered");
        }
        Merger { to_merge, results }
    }

    /// Channel ends a test drives itself: no thread is started.
    #[cfg(test)]
    pub(crate) fn detached() -> (Merger, Sender<MergeResult>, Receiver<MergeJob>) {
        let (to_merge, jobs) = channel();
        let (out, results) = channel();
        (Merger { to_merge, results }, out, jobs)
    }

    /// Queue every `delivered` unit for merging and move it to `merging`.
    /// Priority units are fed first, then fix units, then the rest. Under
    /// the `HOLD-MERGE` flag only fix units and priority units are fed.
    ///
    /// State change only: the merge thread touches the repository, never
    /// the state, so a job sent here is applied later by [`Merger::apply`].
    pub fn feed(&self, ctx: &Ctx, st: &mut State) {
        let hold = ctx.flag("HOLD-MERGE");
        let needs = priority_needs(ctx);
        let holds = dependency_holds(ctx, st);
        let targets = ctx.cfg.target_configs();
        for unit in st.units.values_mut() {
            if unit.status == Status::Held
                && unit.branch.is_some()
                && targets.contains_key(unit.target_id())
            {
                unit.status = Status::Delivered;
            }
        }
        for uid in ranked(ctx, st, st.order.clone()) {
            let dependency = st
                .units
                .get(&uid)
                .and_then(|unit| holds.get(&unit.need))
                .and_then(|deps| deps.first())
                .cloned();
            if note_hold(ctx, st, &uid, Status::Delivered, dependency) {
                continue;
            }
            let Some(unit) = st.units.get_mut(&uid) else {
                continue;
            };
            if hold && !uid.starts_with(&ctx.cfg.fix_prefix) && !needs.contains(&unit.need) {
                continue;
            }
            if unit.status != Status::Delivered {
                continue;
            }
            unit.status = Status::Merging;
            let job = MergeJob {
                uid: uid.clone(),
                target: unit.target_id().to_string(),
                branch: unit.branch.clone().unwrap_or_default(),
                title: unit.title.clone(),
                need: unit.need.clone(),
                proof: unit.proof.clone(),
                tools: unit.tools.clone(),
                write_scope: unit.write_scope.clone(),
            };
            if self.to_merge.send(job).is_err()
                && let Some(unit) = st.units.get_mut(&uid)
            {
                unit.status = Status::Delivered;
            }
        }
    }

    /// Drain finished merge results into state (`apply`): undecided units return to `delivered`, decided failures go
    /// through [`collect::fail`], and merges land with their tip and outside
    /// files — closing the need and appending `needs-ready.txt` when its
    /// last unit just became satisfied.
    pub fn apply(&self, ctx: &Ctx, st: &mut State) {
        while let Ok(result) = self.results.try_recv() {
            match result.ok {
                None => apply_undecided(ctx, st, &result),
                Some(false) => apply_failure(ctx, st, &result),
                Some(true) => apply_merged(ctx, st, &result),
            }
        }
    }
}

/// The worker loop: block on one job — the
/// loop ends when the sender is dropped — drain up to `merge.batch` more,
/// merge the batch, and hand out exactly one result per job. A panic in the
/// work becomes one `merge-error` result per job, never a dead loop.
fn merge_loop(ctx: &Ctx, jobs: &Receiver<MergeJob>, out: &Sender<MergeResult>) {
    let mut queued = std::collections::VecDeque::new();
    loop {
        let first = if let Some(job) = queued.pop_front() {
            job
        } else {
            match jobs.recv() {
                Ok(job) => job,
                Err(_) => break,
            }
        };
        let Some(target_ctx) = ctx.for_target(&first.target) else {
            let result = undecided_result(
                &first.uid,
                "unknown-target",
                &format!("repository target `{}` is not configured", first.target),
            );
            dispatch_results(out, &[first], vec![result]);
            continue;
        };
        let mut batch = vec![first];
        while batch.len() < target_ctx.cfg.merge.batch as usize {
            match jobs.try_recv() {
                Ok(job) if job.target == target_ctx.target_id => batch.push(job),
                Ok(job) => queued.push_back(job),
                Err(_) => break,
            }
        }
        let work = AssertUnwindSafe(|| run_batch(&target_ctx, &batch));
        let done = match catch_unwind(work) {
            Ok(results) => results,
            Err(err) => {
                let detail = panic_text(&err);
                target_ctx.log(&format!("MERGE ERROR {detail}"));
                batch
                    .iter()
                    .map(|j| undecided_result(&j.uid, "merge-error", &detail))
                    .collect()
            }
        };
        dispatch_results(out, &batch, done);
    }
}

/// Offer the batch to the remote merger, else merge here: [`batch_work`]
/// for more than one job, [`merge_work`] for exactly one, then the
/// `batch of <n>: <m> merged` log line.
fn run_batch(ctx: &Ctx, jobs: &[MergeJob]) -> Vec<MergeResult> {
    let done = match remote::offer(ctx, jobs) {
        Some(results) => results,
        None if jobs.len() > 1 => batch_work(ctx, jobs),
        None => vec![merge_work(ctx, &jobs[0])],
    };
    let merged = done.iter().filter(|r| r.ok == Some(true)).count();
    ctx.log(&format!("batch of {}: {merged} merged", jobs.len()));
    done
}

/// Send one result per job results deduplicated by uid,
/// and every job the batch's results never named gets an
/// `ok=None, why="merge-undecided"` result.
fn dispatch_results(out: &Sender<MergeResult>, jobs: &[MergeJob], done: Vec<MergeResult>) {
    let mut seen: Vec<String> = Vec::new();
    for result in done {
        if seen.contains(&result.uid) {
            continue;
        }
        seen.push(result.uid.clone());
        if out.send(result).is_err() {
            return;
        }
    }
    for job in jobs {
        if seen.iter().any(|uid| uid == &job.uid) {
            continue;
        }
        let result = undecided_result(&job.uid, "merge-undecided", "");
        if out.send(result).is_err() {
            return;
        }
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

/// The panic payload as one line of at most 600 chars, like python's
/// flattened traceback tail.
fn panic_text(err: &Box<dyn Any + Send>) -> String {
    let text = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| String::from("unknown panic"));
    tail(&text.replace('\n', " | "), 600)
}

/// `ok=null`: the unit returns to `delivered` for a later retry, and the
/// log line is `<why> <uid>: <first 200 chars of detail>`.
fn apply_undecided(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    if result.why.as_deref() == Some("unknown-target") {
        let detail = result
            .detail
            .as_deref()
            .unwrap_or("repository target is not configured");
        if let Some(unit) = st.units.get_mut(&result.uid) {
            unit.status = Status::Held;
        }
        ctx.exception("unknown-target", &result.uid, detail);
        return;
    }
    if let Some(unit) = st.units.get_mut(&result.uid) {
        unit.status = Status::Delivered;
    }
    let why = result.why.as_deref().unwrap_or("");
    let head: String = result
        .detail
        .clone()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    ctx.log(&format!("{why} {}: {head}", result.uid));
}

/// `ok=false`: a decided failure — the unit is failed through
/// [`collect::fail`], which retries it or marks it stuck.
fn apply_failure(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    let why = result.why.clone().unwrap_or_default();
    let detail = result.detail.clone().unwrap_or_default();
    collect::fail(ctx, st, &result.uid, &why, &detail);
}

/// `ok=true`: the unit becomes `merged` with its tip and outside files, the
/// merge counter ticks, and the `scope-warn` and `merged` lines are logged.
fn apply_merged(ctx: &Ctx, st: &mut State, result: &MergeResult) {
    let now = ctx.now();
    if let Some(unit) = st.units.get_mut(&result.uid) {
        unit.status = Status::Merged;
        unit.commit.clone_from(&result.tip);
        unit.merged_at = Some(now);
        unit.since = now;
        unit.outside.clone_from(&result.outside);
    }
    st.merges += 1;
    if !result.outside.is_empty() {
        let files: String = result.outside.join(" ").chars().take(300).collect();
        ctx.log(&format!(
            "scope-warn {} touched outside scope: {files}",
            result.uid
        ));
    }
    let tip: String = result
        .tip
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(10)
        .collect();
    ctx.log(&format!(
        "merged {} tip={} in {}s",
        result.uid,
        tip,
        result.secs.unwrap_or(0)
    ));
    close_need(ctx, st, &result.uid, &tip);
}

/// Close the unit's need and append its `needs-ready.txt` line when the
/// need record exists, is not closed yet, and every one of its units is
/// satisfied.
fn close_need(ctx: &Ctx, st: &mut State, uid: &str, tip: &str) {
    let Some(need_id) = st.units.get(uid).map(|u| u.need.clone()) else {
        return;
    };
    let Some(need) = st.needs.get(&need_id) else {
        return;
    };
    if st.closed_needs.contains(&need_id) || !need.units.iter().all(|x| satisfied(st, x, 0)) {
        return;
    }
    let units = need.units.join(",");
    st.closed_needs.push(need_id.clone());
    let line = format!("{need_id} {tip} {units}\n");
    if append_needs_ready(ctx, &line).is_err() {
        ctx.log(&format!("needs-ready: cannot append {need_id}"));
    }
}

/// Append one line to `<home>/needs-ready.txt`, creating the file when it
/// does not exist yet.
fn append_needs_ready(ctx: &Ctx, line: &str) -> std::io::Result<()> {
    let path = ctx.home.join("needs-ready.txt");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())
}
