//! Need branches: names, the base recorded once per run, and the tip.
//!
//! There is one need branch per need (the `need_branches` table is keyed by
//! need), on one target: the row records the target of the need's first
//! slice, and every later read uses that target.

use crate::error::VivariumError;
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::verify::remote_tip;
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::store::{NeedBranchRow, PumpStore};

/// No base yet for the row's run.
pub const FRESH: &str = "fresh";
/// A base is recorded for the row's run.
pub const LIVE: &str = "live";
/// The old branch is being moved to its prior name.
pub const RENAMING: &str = "renaming";
/// The old branch name is being deleted after the move.
pub const DELETING: &str = "deleting";
/// The need landed and its refs were cleaned up.
pub const PRUNED: &str = "pruned";

/// The need branch of `need`.
#[must_use]
pub fn branch(need: &str) -> String {
    format!("vivi/need/{need}")
}

/// The name a need branch is moved to when run `run` is restarted. One
/// segment after the `vivi/need-prior/` prefix, so the cleanup rule accepts it.
#[must_use]
pub fn prior(need: &str, run: u32) -> String {
    format!("vivi/need-prior/{need}-r{run}")
}

fn target_ctx(ctx: &Ctx, target: &str) -> Result<Ctx, VivariumError> {
    ctx.for_target(target)
        .ok_or_else(|| VivariumError::Config(format!("unknown target {target}")))
}

fn ls_remote(target: &Ctx, remote_ref: &str) -> Result<Option<String>, VivariumError> {
    remote_tip(target, &target.cfg.remote, remote_ref).map_err(VivariumError::Other)
}

/// Record the base of `need` for run `run`: the target's main tip, read once.
///
/// A `live` row for this run returns its base unchanged. A `renaming` or
/// `deleting` row is an error: the old branch is still being moved. Any other
/// row (or none) is written as `live` with the current main tip, keeping its
/// `priors`.
///
/// # Errors
/// Returns [`VivariumError`] for an unknown target, a moving old branch, a
/// missing main ref, or a failed ls-remote or store write.
pub fn record_base(
    ctx: &Ctx,
    store: &PumpStore,
    need: &str,
    run: u32,
    target: &str,
) -> Result<String, VivariumError> {
    let existing = store.need_branch(need)?;
    if let Some(row) = &existing {
        if row.status == RENAMING || row.status == DELETING {
            return Err(VivariumError::Other(format!(
                "need branch of {need} is {}: the old branch is still being moved",
                row.status
            )));
        }
        if row.status == LIVE
            && row.run == run
            && let Some(base) = &row.base
        {
            return Ok(base.clone());
        }
    }
    let target_ctx = target_ctx(ctx, target)?;
    let main = target_ctx.cfg.main.clone();
    let base = ls_remote(&target_ctx, &main)?.ok_or_else(|| {
        VivariumError::Other(format!("main ref {main} does not exist on target {target}"))
    })?;
    store.upsert_need_branch(&NeedBranchRow {
        need: need.to_owned(),
        branch: branch(need),
        base: Some(base.clone()),
        status: LIVE.to_owned(),
        run,
        target: target.to_owned(),
        priors: existing.map(|row| row.priors).unwrap_or_default(),
    })?;
    Ok(base)
}

/// The tip of the need branch: for a `live` row, the remote tip of
/// `vivi/need/<need>` on the row's target when the ref exists, else the base.
/// `None` for no row or any other status.
///
/// # Errors
/// Returns [`VivariumError`] for an unknown target or a failed ls-remote.
pub fn tip(ctx: &Ctx, store: &PumpStore, need: &str) -> Result<Option<String>, VivariumError> {
    let Some(row) = store.need_branch(need)? else {
        return Ok(None);
    };
    if row.status != LIVE {
        return Ok(None);
    }
    let target = target_ctx(ctx, &row.target)?;
    match ls_remote(&target, &format!("refs/heads/{}", row.branch))? {
        Some(sha) => Ok(Some(sha)),
        None => Ok(row.base),
    }
}

/// Registry hook: N4a-20 fills the need-branch functions and leaves this empty.
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}

#[cfg(test)]
#[path = "need_branch_test.rs"]
mod tests;
