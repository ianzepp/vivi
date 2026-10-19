//! The batch merge path: several unit branches merged together, checked
//! once, published once. A failing unit proof isolates its unit; a failing
//! shared check sets the whole batch aside (quarantine).

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::quarantine;
use crate::pump::state::{MergeJob, MergeResult, Quarantine};

use super::fetch::fetch_unit_branch;
use super::publish::publish;
use super::{
    GIT_TIMEOUT, already_integrated, check_with_drift_retry, failed, failure_text, fetch_failure,
    flat, format_style_commit, identity_args, integrated_text, merge_line, merge_work,
    outside_files, reset_repo, resolve_generated, skips_proof, tail, undecided_result,
};

/// A cleanly merged job plus the files its merge touched outside its scope.
struct Merged {
    job: MergeJob,
    outside: Vec<String>,
}

/// What a shared check decided about the batch.
enum SharedOutcome {
    /// Every shared check passed; keep going.
    Pass,
    /// A shared check failed and decided the batch: one proof-failed result
    /// for a single job, or quarantine results (and no pushed integration
    /// branch) for several.
    Failed(Vec<MergeResult>),
}

/// Merge a batch of unit branches together, check the result once and push
/// once. Returns the results for the decided jobs and the jobs a second round
/// should retry (empty when none remain). A failing shared check decides the
/// whole batch, so it leaves nothing to retry.
#[must_use]
pub fn batch_round(ctx: &Ctx, jobs: &[MergeJob]) -> (Vec<MergeResult>, Vec<MergeJob>) {
    let t0 = ctx.now();
    reset_repo(ctx);
    merge_main_in(ctx);
    let (mut results, merged) = merge_units(ctx, jobs);
    if merged.is_empty() {
        return (results, Vec::new());
    }
    let tag = style_tag(&merged);
    format_style_commit(ctx, &tag);
    match run_shared(ctx, &merged) {
        SharedOutcome::Pass => {}
        SharedOutcome::Failed(decided) => {
            results.extend(decided);
            return (results, Vec::new());
        }
    }
    let culprits = run_proofs(ctx, &merged, &mut results);
    if !culprits.is_empty() {
        let rest: Vec<MergeJob> = merged
            .into_iter()
            .map(|m| m.job)
            .filter(|j| !culprits.contains(&j.uid))
            .collect();
        return (results, rest);
    }
    results.extend(push_batch(ctx, merged, t0));
    (results, Vec::new())
}

/// Drive the batch rounds: one round over all jobs, a second over the
/// survivors of a failing unit proof, and any job still undecided after that
/// merged one at a time with [`merge_work`].
#[must_use]
pub fn batch_work(ctx: &Ctx, jobs: &[MergeJob]) -> Vec<MergeResult> {
    let (mut out, rest) = batch_round(ctx, jobs);
    if rest.is_empty() {
        return out;
    }
    let (more, leftover) = batch_round(ctx, &rest);
    out.extend(more);
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
    for (i, job) in jobs.iter().enumerate() {
        if let Some(result) = merge_one(ctx, job, (i + 1, jobs.len())) {
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

/// Fetch and merge job number `at.0` of `at.1`. `None` means it merged
/// cleanly.
fn merge_one(ctx: &Ctx, job: &MergeJob, at: (usize, usize)) -> Option<MergeResult> {
    let out = fetch_unit_branch(ctx, &job.branch);
    if out.code != 0 {
        merge_line(
            ctx,
            &job.uid,
            at,
            &format!("fetch failed: {}", flat(&out.text)),
        );
        return Some(fetch_failure(&job.uid, &out.text));
    }
    if let Some(result) = already_integrated(ctx, job, ctx.now()) {
        merge_line(ctx, &job.uid, at, &integrated_text(&result));
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
        merge_line(ctx, &job.uid, at, "ok");
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
        merge_line(ctx, &job.uid, at, "generated");
        return None;
    }
    merge_line(ctx, &job.uid, at, "conflict");
    drop(ctx.git("merge --abort", GIT_TIMEOUT));
    let detail = format!(
        "The branch no longer merges into the integration branch. Conflicting files: {raw}"
    );
    Some(failed(&job.uid, "merge-conflict", detail))
}

/// Run the shared checks once over the whole batch, each with a
/// `batch <n> shared rc=<n> <n>s: <cmd>` log line.
fn run_shared(ctx: &Ctx, merged: &[Merged]) -> SharedOutcome {
    let total = ctx.cfg.merge.shared_checks.len();
    let tag = format!("batch of {}", merged.len());
    for (i, cmd) in ctx.cfg.merge.shared_checks.iter().enumerate() {
        ctx.step(&format!("check {}/{total} start: {cmd}", i + 1));
        let t1 = ctx.now();
        let out = check_with_drift_retry(ctx, &tag, cmd);
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

/// Decide a failed shared check. The failure text is logged first: one
/// merged job is the culprit and fails with `proof-failed`; several are set
/// aside together (see [`quarantine_batch`]).
fn shared_failure(ctx: &Ctx, merged: &[Merged], cmd: &str, code: i32, text: &str) -> SharedOutcome {
    log_failure(ctx, text);
    if merged.len() == 1 {
        reset_repo(ctx);
        let detail = proof_detail(code, cmd, text);
        let result = failed(&merged[0].job.uid, "proof-failed", detail);
        return SharedOutcome::Failed(vec![result]);
    }
    SharedOutcome::Failed(quarantine_batch(ctx, merged, cmd, code, text))
}

/// Log the last 40 lines of a failed shared check's distilled output, one
/// log line each.
fn log_failure(ctx: &Ctx, text: &str) {
    let distilled = failure_text(text);
    let lines: Vec<&str> = distilled.lines().collect();
    for line in &lines[lines.len().saturating_sub(40)..] {
        ctx.log(&format!("  {line}"));
    }
}

/// Set a batch aside after a shared check failed: push the lane's `HEAD`
/// (the merged batch and its style commit) to a new `pump/quarantine/<id>`
/// branch, reset the lane, and report every merged job as quarantined. A push
/// that did not land leaves every job undecided, to be fed again later.
fn quarantine_batch(
    ctx: &Ctx,
    merged: &[Merged],
    cmd: &str,
    code: i32,
    text: &str,
) -> Vec<MergeResult> {
    let uids: Vec<&str> = merged.iter().map(|m| m.job.uid.as_str()).collect();
    let id = quarantine::new_id(ctx, &uids);
    let push = format!(
        "push {} HEAD:refs/heads/{}",
        ctx.cfg.remote,
        quarantine::branch(&id)
    );
    let out = ctx.git(&push, 180);
    reset_repo(ctx);
    if out.code != 0 {
        let detail = tail(&out.text, 200);
        ctx.log(&format!("quarantine {id}: push failed: {detail}"));
        return merged
            .iter()
            .map(|m| undecided_result(&m.job.uid, "quarantine-push-failed", &detail))
            .collect();
    }
    ctx.log(&format!(
        "quarantine {id}: {} units, {cmd} rc={code}",
        uids.len()
    ));
    let info = Quarantine {
        id: id.clone(),
        cmd: cmd.to_string(),
        code,
        text: failure_text(text),
        units: uids.iter().map(|uid| (*uid).to_string()).collect(),
    };
    uids.iter()
        .map(|uid| MergeResult {
            uid: (*uid).to_string(),
            why: Some(String::from("quarantined")),
            detail: Some(id.clone()),
            quarantine: Some(info.clone()),
            ..MergeResult::default()
        })
        .collect()
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
            let head: String = cmd.chars().take(90).collect();
            ctx.step(&format!("proof {} start: {head}", m.job.uid));
            let t1 = ctx.now();
            let out = check_with_drift_retry(ctx, &m.job.uid, cmd);
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

/// Publish the batch once and shape one result per merged job. A publish
/// that did not land leaves every merged job undecided.
fn push_batch(ctx: &Ctx, merged: Vec<Merged>, t0: u64) -> Vec<MergeResult> {
    let tip = match publish(ctx) {
        Ok(tip) => tip,
        Err(failure) => {
            return merged
                .into_iter()
                .map(|m| undecided_result(&m.job.uid, failure.why, &failure.detail))
                .collect();
        }
    };
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
            quarantine: None,
        })
        .collect()
}
