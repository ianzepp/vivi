//! Publishing a lane's merged `HEAD` to the integration branch.
//!
//! Merge lanes run side by side, so the remote tip may have moved while a
//! lane was merging and checking. The publish routine absorbs a moved tip
//! with a plain merge (no re-run of the checks: a cross-lane break is the
//! gate's job) and retries a push the remote rejected.

use crate::pump::host::{Ctx, shell_quote};

use super::{GIT_TIMEOUT, identity_args, reset_repo, tail};

/// Fetch, merge and push rounds before a publish gives up as undecided.
const ATTEMPTS: u32 = 3;

/// Why a publish did not land. The batch's jobs come back as undecided
/// results and return to `delivered`.
#[derive(Debug)]
pub(super) struct PublishFailure {
    /// Short reason code, like `push-failed`.
    pub why: &'static str,
    /// What git said.
    pub detail: String,
}

/// Push the lane's `HEAD` to `remote/branch` and return the pushed tip.
///
/// Each round fetches the integration branch first; a remote tip that is not
/// an ancestor of `HEAD` is merged in (no-ff, pump identity). A clean merge
/// is pushed without re-running checks. A merge conflict aborts and fails the
/// publish at once; a rejected push repeats the round, up to three times.
/// Every failure leaves the checkout reset to the remote tip.
pub(super) fn publish(ctx: &Ctx) -> Result<String, PublishFailure> {
    discard_dirt(ctx);
    let target = format!("HEAD:refs/heads/{}", ctx.cfg.branch);
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        sync_remote_tip(ctx)?;
        ctx.step(&format!("publish: push attempt {attempt}"));
        let out = ctx.git(&format!("push {} {target}", ctx.cfg.remote), 180);
        if out.code == 0 {
            let tip = ctx
                .git("rev-parse HEAD", GIT_TIMEOUT)
                .text
                .trim()
                .to_string();
            ctx.log(&format!("publish: pushed {}", short(&tip)));
            return Ok(tip);
        }
        ctx.log(&format!(
            "push of {} rejected (attempt {attempt}/{ATTEMPTS})",
            ctx.cfg.branch
        ));
        last = out.text;
    }
    reset_repo(ctx);
    ctx.log("publish: failed push-failed");
    Err(PublishFailure {
        why: "push-failed",
        detail: tail(&last, 200),
    })
}

/// The first ten characters of a commit id.
fn short(sha: &str) -> String {
    sha.trim().chars().take(10).collect()
}

/// Drop whatever the checks left in the working tree.
fn discard_dirt(ctx: &Ctx) {
    let dirty = ctx.git("status --porcelain", GIT_TIMEOUT).text;
    if !dirty.trim().is_empty() {
        drop(ctx.git("checkout -q -- .", GIT_TIMEOUT));
    }
}

/// Fetch the integration branch and merge its tip into `HEAD` when another
/// lane pushed meanwhile.
fn sync_remote_tip(ctx: &Ctx) -> Result<(), PublishFailure> {
    let remote = ctx.cfg.remote.as_str();
    let branch = ctx.cfg.branch.as_str();
    ctx.step("publish: fetch");
    let fetch = ctx.git(&format!("fetch -q {remote} {branch}"), 180);
    if fetch.code != 0 {
        return Err(abandon(ctx, "publish-fetch-failed", &fetch.text));
    }
    let tip = format!("{remote}/{branch}");
    let check = ctx.git(&format!("merge-base --is-ancestor {tip} HEAD"), GIT_TIMEOUT);
    match check.code {
        0 => Ok(()),
        1 => merge_remote_tip(ctx, &tip),
        _ => Err(abandon(ctx, "publish-check-failed", &check.text)),
    }
}

/// Merge the moved remote tip into `HEAD`; a conflict aborts the merge and
/// fails the publish.
fn merge_remote_tip(ctx: &Ctx, tip: &str) -> Result<(), PublishFailure> {
    let sha = ctx.git(&format!("rev-parse {tip}"), GIT_TIMEOUT).text;
    ctx.step(&format!(
        "publish: merging moved {} tip {}",
        ctx.cfg.branch,
        short(&sha)
    ));
    let msg = shell_quote(&format!("merge {tip} into lane"));
    let cmd = format!(
        "{} merge --no-ff --no-edit -m {msg} {tip}",
        identity_args(ctx)
    );
    let out = ctx.git(&cmd, GIT_TIMEOUT);
    if out.code == 0 {
        ctx.log(&format!("merged moved {tip} into lane before push"));
        return Ok(());
    }
    let files = ctx
        .git("diff --name-only --diff-filter=U", GIT_TIMEOUT)
        .text;
    let detail = format!("The moved integration tip {tip} conflicts with this batch: {files}");
    Err(abandon(ctx, "publish-conflict", &detail))
}

/// Reset the checkout and shape the failure.
fn abandon(ctx: &Ctx, why: &'static str, detail: &str) -> PublishFailure {
    ctx.log(&format!("publish: failed {why}"));
    reset_repo(ctx);
    PublishFailure {
        why,
        detail: detail.chars().take(400).collect(),
    }
}
