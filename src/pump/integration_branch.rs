//! The pump's integration branch. A target whose `main` names a pump branch
//! (`vivi/main`) lands needs there and leaves the upstream branch alone. The
//! branch is created on the target's remote, from the remote's default
//! branch, the first time the pump finds it missing.

use crate::VivariumError;
use crate::pump::flow::verify::remote_tip;
use crate::pump::host::{Ctx, shell_quote};

const GIT_SECS: u64 = 120;
const QUERY_SECS: u64 = 30;

/// The branch namespace the pump owns. A target `main` outside it names a
/// branch somebody else keeps, and a missing one is a configuration error to
/// report, not a branch to invent.
pub(crate) const PUMP_BRANCH_PREFIX: &str = "vivi/";

/// Make sure every target's integration branch exists on its remote.
///
/// # Errors
/// Returns the first target's failure after trying all of them.
pub(crate) fn ensure_all(ctx: &Ctx) -> Result<(), VivariumError> {
    let mut first = None;
    for id in ctx.cfg.target_configs().keys() {
        let Some(target) = ctx.for_target(id) else {
            continue;
        };
        if let Err(error) = ensure(&target)
            && first.is_none()
        {
            first = Some(VivariumError::Other(format!("target {id}: {error}")));
        }
    }
    first.map_or(Ok(()), Err)
}

/// Create the target's `main` on its remote when it is a pump branch and is
/// missing. Does nothing when the branch exists, when `main` is not a pump
/// branch, or when the remote has no default branch to start from (an empty
/// repository).
fn ensure(tctx: &Ctx) -> Result<(), String> {
    let main = tctx.cfg.main.as_str();
    if !main.starts_with(PUMP_BRANCH_PREFIX) {
        return Ok(());
    }
    let remote = tctx.cfg.remote.as_str();
    if remote_tip(tctx, remote, main)?.is_some() {
        return Ok(());
    }
    let Some(default) = default_branch(tctx, remote)? else {
        return Ok(());
    };
    let fetch = tctx.git(
        &format!("fetch {} {}", shell_quote(remote), shell_quote(&default)),
        GIT_SECS,
    );
    if fetch.code != 0 {
        return Err(format!("fetch {default}: {}", fetch.text.trim()));
    }
    let push = tctx.git(
        &format!(
            "push {} FETCH_HEAD:refs/heads/{}",
            shell_quote(remote),
            shell_quote(main)
        ),
        GIT_SECS,
    );
    if push.code != 0 {
        return Err(format!("push {main}: {}", push.text.trim()));
    }
    tctx.log(&format!(
        "created integration branch {main} on {remote} from {default}"
    ));
    Ok(())
}

/// The branch the remote's `HEAD` points at, or `None` when it points at
/// nothing (an empty repository).
fn default_branch(tctx: &Ctx, remote: &str) -> Result<Option<String>, String> {
    let out = tctx.git(
        &format!("ls-remote --symref {} HEAD", shell_quote(remote)),
        QUERY_SECS,
    );
    if out.code != 0 {
        return Err(format!("ls-remote exit {}: {}", out.code, out.text.trim()));
    }
    Ok(out.text.lines().find_map(|line| {
        let target = line.strip_prefix("ref: refs/heads/")?;
        let (branch, _) = target.split_once(char::is_whitespace)?;
        Some(branch.to_string())
    }))
}

#[cfg(test)]
#[path = "integration_branch_test.rs"]
mod tests;
