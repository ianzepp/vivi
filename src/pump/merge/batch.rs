//! The batch merge path: several unit branches merged together, checked
//! once, published once, with culprit isolation.

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::{MergeJob, MergeResult};

use super::publish::publish;
use super::{
    GIT_TIMEOUT, already_integrated, failed, failure_text, fetch_failure, format_style_commit,
    identity_args, merge_work, outside_files, reset_repo, skips_proof, tail, undecided_result,
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
        })
        .collect()
}
