//! Side-effect verification: did a step's git effect land? Facts come from git
//! and the runner publish receipt, never from a model's envelope.

use crate::pump::host::{Ctx, shell_quote};

/// Seconds a read-only git query may take.
const QUERY_SECS: u64 = 30;
/// Seconds a fetch may take.
const FETCH_SECS: u64 = 120;

/// The publish receipt a runner wrote: the SHA it says it pushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerPublishReceipt {
    pub sha: String,
}

/// What to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReq {
    /// Target id; its repository and remote are the ones queried.
    pub target: String,
    /// The remote ref the step should have moved (`main`, `refs/heads/<need>`).
    pub remote_ref: String,
    /// The ref's SHA before the step started; `None` when the ref did not exist.
    pub before: Option<String>,
    /// A commit the ref must contain once the step landed.
    pub must_contain: String,
    /// The runner's publish receipt, when one was written.
    pub receipt: Option<RunnerPublishReceipt>,
}

/// What git says about the step's side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideEffect {
    /// The effect landed; `sha` is the published or current ref SHA.
    Verified { sha: String },
    /// The ref did not move: the step did nothing.
    Absent,
    /// Git could not settle it, or the evidence conflicts.
    Unclear { reason: String },
}

fn unclear(reason: impl Into<String>) -> SideEffect {
    SideEffect::Unclear {
        reason: reason.into(),
    }
}

fn valid_sha(sha: &str) -> bool {
    (4..=64).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Verify one side effect against git and the receipt.
#[must_use]
pub fn check(ctx: &Ctx, req: &VerifyReq) -> SideEffect {
    let Some(target) = ctx.for_target(&req.target) else {
        return unclear(format!("unknown target {}", req.target));
    };
    let remote = target.cfg.remote.clone();
    if !valid_sha(&req.must_contain) {
        return unclear("must_contain is not a commit SHA");
    }
    let tip = match remote_tip(&target, &remote, &req.remote_ref) {
        Ok(tip) => tip,
        Err(reason) => return unclear(reason),
    };
    if let Some(receipt) = &req.receipt {
        match receipt_lands(
            &target,
            &remote,
            &req.remote_ref,
            &receipt.sha,
            tip.as_deref(),
        ) {
            Ok(true) => {
                return SideEffect::Verified {
                    sha: receipt.sha.clone(),
                };
            }
            Ok(false) => {}
            Err(reason) => return unclear(reason),
        }
    }
    match (
        by_ref_state(&target, req, &remote, tip.as_deref()),
        &req.receipt,
    ) {
        (SideEffect::Absent, Some(receipt)) => unclear(format!(
            "receipt names {} but {} did not move",
            receipt.sha, req.remote_ref
        )),
        (other, _) => other,
    }
}

/// The remote ref's SHA, `None` when the remote does not have the ref.
pub(crate) fn remote_tip(
    ctx: &Ctx,
    remote: &str,
    remote_ref: &str,
) -> Result<Option<String>, String> {
    let out = ctx.git(
        &format!(
            "ls-remote {} {}",
            shell_quote(remote),
            shell_quote(remote_ref)
        ),
        QUERY_SECS,
    );
    if out.code != 0 {
        return Err(format!("ls-remote exit {}: {}", out.code, out.text.trim()));
    }
    let full = format!("refs/heads/{remote_ref}");
    for line in out.text.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(sha), Some(name)) = (parts.next(), parts.next())
            && (name == remote_ref || name == full)
        {
            return if valid_sha(sha) {
                Ok(Some(sha.to_owned()))
            } else {
                Err(format!("ls-remote returned a bad SHA {sha}"))
            };
        }
    }
    Ok(None)
}

/// Rule 1: the receipt's SHA is the ref tip, or an ancestor of it.
fn receipt_lands(
    ctx: &Ctx,
    remote: &str,
    remote_ref: &str,
    sha: &str,
    tip: Option<&str>,
) -> Result<bool, String> {
    if !valid_sha(sha) {
        return Err(String::from("receipt SHA is not a commit SHA"));
    }
    let Some(tip) = tip else {
        return Ok(false);
    };
    if tip == sha {
        return Ok(true);
    }
    fetch(ctx, remote, remote_ref)?;
    is_ancestor(ctx, sha, tip)
}

/// Rule 2: compare the ref with its state before the step.
fn by_ref_state(ctx: &Ctx, req: &VerifyReq, remote: &str, tip: Option<&str>) -> SideEffect {
    let Some(tip) = tip else {
        return match req.before {
            None => SideEffect::Absent,
            Some(_) => unclear(format!("{} is gone from {remote}", req.remote_ref)),
        };
    };
    if req.before.as_deref() == Some(tip) {
        return SideEffect::Absent;
    }
    if let Err(reason) = fetch(ctx, remote, &req.remote_ref) {
        return unclear(reason);
    }
    match is_ancestor(ctx, &req.must_contain, tip) {
        Ok(true) => SideEffect::Verified {
            sha: tip.to_owned(),
        },
        Ok(false) => unclear(format!(
            "{} moved to {tip} without {}",
            req.remote_ref, req.must_contain
        )),
        Err(reason) => unclear(reason),
    }
}

fn fetch(ctx: &Ctx, remote: &str, remote_ref: &str) -> Result<(), String> {
    let out = ctx.git(
        &format!("fetch {} {}", shell_quote(remote), shell_quote(remote_ref)),
        FETCH_SECS,
    );
    if out.code == 0 {
        Ok(())
    } else {
        Err(format!("fetch exit {}: {}", out.code, out.text.trim()))
    }
}

/// Whether the commit exists in the local object store.
fn commit_present(ctx: &Ctx, sha: &str) -> bool {
    ctx.git(&format!("cat-file -e {sha}^{{commit}}"), QUERY_SECS)
        .code
        == 0
}

/// `git merge-base --is-ancestor`: 0 yes, 1 no, anything else is an error.
fn is_ancestor(ctx: &Ctx, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let out = ctx.git(
        &format!("merge-base --is-ancestor {ancestor} {descendant}"),
        QUERY_SECS,
    );
    match out.code {
        0 => Ok(true),
        1 => Ok(false),
        code => Err(format!("merge-base exit {code}: {}", out.text.trim())),
    }
}

/// The remote ref's tip when it already contains `sha`: `Ok(Some(tip))` when
/// `sha` is the tip or an ancestor of it, `Ok(None)` when the ref is absent or
/// does not contain `sha`. Unlike `check`, a moved ref without the commit is a
/// plain "no", not Unclear.
pub(crate) fn ref_contains(
    ctx: &Ctx,
    target: &str,
    remote_ref: &str,
    sha: &str,
) -> Result<Option<String>, String> {
    let Some(tctx) = ctx.for_target(target) else {
        return Err(format!("unknown target {target}"));
    };
    if !valid_sha(sha) {
        return Err(String::from("sha is not a commit SHA"));
    }
    let remote = tctx.cfg.remote.clone();
    let Some(tip) = remote_tip(&tctx, &remote, remote_ref)? else {
        return Ok(None);
    };
    if tip == sha {
        return Ok(Some(tip));
    }
    fetch(&tctx, &remote, remote_ref)?;
    // The fetch brings every ancestor of the tip, so a commit still absent
    // afterwards is not contained (and merge-base would exit 128).
    if !commit_present(&tctx, sha) {
        return Ok(None);
    }
    Ok(is_ancestor(&tctx, sha, &tip)?.then_some(tip))
}

#[cfg(test)]
#[path = "verify_test.rs"]
mod tests;

#[cfg(test)]
#[path = "verify_contains_test.rs"]
mod contains_tests;
