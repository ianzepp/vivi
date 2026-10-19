//! Getting a unit branch into a merge checkout.
//!
//! A unit branch normally lives on the target's remote. A local-worktree
//! target (`cfg.targets` set) commits its result branch straight into the
//! target's own integration repository instead and never pushes it. Lane 0
//! works in that repository, so such a branch is already local there. Every
//! other lane is a separate `--reference` clone, which cannot see it; a
//! second remote named [`BASE_REMOTE`] in the clone points back at the base
//! repository so the branch can be fetched from there. A unit from a `local`
//! worktree pool names its own pool repository in `source`; that branch is
//! fetched straight from the pool path.

use std::path::{Path, PathBuf};

use crate::pump::host::{CmdOutput, Ctx, shell_quote};
use crate::pump::state::MergeJob;

/// The remote in a lane clone that points at the target's own repository.
pub(super) const BASE_REMOTE: &str = "pump-base";

/// Put the unit branch `job.branch` at `refs/remotes/<remote>/<branch>` in the
/// checkout of `ctx`, ready to merge. Where the branch comes from:
///
/// * a `local` worktree pool unit: `job.source`, the pool's own repository;
/// * a local-worktree target, lane 0: the local branch of the checkout;
/// * a local-worktree target, any other lane: the base repository, through
///   [`BASE_REMOTE`], when it has the branch there;
/// * everything else: the configured remote.
///
/// Used by both [`super::merge_work`] and the batch path, so they agree on
/// where a branch can be found.
#[must_use]
pub(crate) fn fetch_unit_branch(ctx: &Ctx, job: &MergeJob) -> CmdOutput {
    let b = shell_quote(&job.branch);
    let remote = ctx.cfg.remote.as_str();
    if let Some(source) = &job.source {
        let path = shell_quote(&source.to_string_lossy());
        let cmd = format!("fetch {path} +refs/heads/{b}:refs/remotes/{remote}/{b}");
        return ctx.git(&cmd, 180);
    }
    if !ctx.cfg.targets.is_empty() {
        match lane_base(ctx) {
            None if is_local(ctx, &b) => {
                let cmd = format!("update-ref refs/remotes/{remote}/{b} refs/heads/{b}");
                return ctx.git(&cmd, 180);
            }
            Some(base) if ensure_base_remote(ctx, &base) && on_base(ctx, &b) => {
                let cmd = format!("fetch {BASE_REMOTE} +refs/heads/{b}:refs/remotes/{remote}/{b}");
                return ctx.git(&cmd, 180);
            }
            _ => {}
        }
    }
    ctx.git(
        &format!("fetch {remote} refs/heads/{b}:refs/remotes/{remote}/{b}"),
        180,
    )
}

/// The base repository of the target when `ctx` is a separate lane clone of
/// it; `None` for lane 0 (the base itself) and for a context with no
/// configured target.
fn lane_base(ctx: &Ctx) -> Option<PathBuf> {
    let base = ctx.cfg.target_repo_dir(&ctx.project, &ctx.target_id)?;
    (base != ctx.repo).then_some(base)
}

/// True when the checkout has a local branch `b` (already shell-quoted).
fn is_local(ctx: &Ctx, b: &str) -> bool {
    let cmd = format!("show-ref --verify --quiet refs/heads/{b}");
    ctx.git(&cmd, 30).code == 0
}

/// True when the base repository has the branch `b` (already shell-quoted).
fn on_base(ctx: &Ctx, b: &str) -> bool {
    let cmd = format!("ls-remote --exit-code {BASE_REMOTE} refs/heads/{b}");
    ctx.git(&cmd, 60).code == 0
}

/// Make sure the checkout of `ctx` has the [`BASE_REMOTE`] remote pointing at
/// `base`, adding it when missing. False when it could not be added.
pub(super) fn ensure_base_remote(ctx: &Ctx, base: &Path) -> bool {
    if ctx.git(&format!("remote get-url {BASE_REMOTE}"), 30).code == 0 {
        return true;
    }
    let path = shell_quote(&base.to_string_lossy());
    ctx.git(&format!("remote add {BASE_REMOTE} {path}"), 30)
        .code
        == 0
}
