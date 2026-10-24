//! Reclaim disposable pool build output.
//!
//! **2x rule.** A slot's build directory (configured `build_dir`, plus Cargo
//! `target/`) is kept warm between jobs. When its size exceeds twice the
//! post-fresh-build baseline (`marks/build_baseline`, or 4 GiB when the mark
//! is missing) the contents are deleted. The next job rebuilds cold.
//!
//! Never touches a slot that is claimed or whose boot process is alive.

use std::path::{Path, PathBuf};

use super::clean_rule::{DISK_RECLAIM_GIB, DISK_REFUSE_GIB, GIB, format_bytes, volume_free_bytes};
use super::clean_slot::{self, slot_is_free};
use super::pools::{self, Need, PoolHandle};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;

pub mod rule {
    pub use super::super::clean_rule::*;
}

/// Kind of reclaimable residue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Build,
    Reset,
}

impl ActionKind {
    /// Porcelain token.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Reset => "reset",
        }
    }
}

/// One reclaim (or dry-run) the cleaner performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanAction {
    pub pool: String,
    pub slot: String,
    pub kind: ActionKind,
    pub bytes: u64,
    pub path: String,
    pub note: String,
}

/// One path left alone, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanSkip {
    pub pool: String,
    pub slot: String,
    pub kind: ActionKind,
    pub reason: String,
}

/// What [`clean_mailspace`] did, or would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanReport {
    pub actions: Vec<CleanAction>,
    pub skips: Vec<CleanSkip>,
    pub free_bytes: Option<u64>,
    pub refused: Option<String>,
}

impl CleanReport {
    /// Bytes named on the action list.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.actions.iter().map(|a| a.bytes).sum()
    }

    fn take(&mut self, pair: (Vec<CleanAction>, Vec<CleanSkip>)) {
        self.actions.extend(pair.0);
        self.skips.extend(pair.1);
    }
}

/// Options for [`clean_mailspace`].
#[derive(Debug, Clone, Default)]
pub struct CleanOptions {
    pub dry_run: bool,
    pub pool: Option<String>,
    pub base: Option<String>,
}

/// Reclaim free slots of every selected local pool; reset free remote slots
/// onto `--base` when given.
///
/// # Errors
/// Returns an error if a pool cannot be opened or a reclaim fails hard.
pub fn clean_mailspace(
    space: &Mailspace,
    options: &CleanOptions,
) -> Result<CleanReport, VivariumError> {
    let handles = pools::open(space, options.pool.as_deref(), Need::Any)?;
    let mut report = CleanReport::default();
    for handle in &handles {
        report.take(clean_pool(
            handle,
            space,
            options.base.as_deref(),
            options.dry_run,
        )?);
    }
    report.free_bytes = handles
        .iter()
        .find(|h| h.config.provider == "local")
        .and_then(|h| h.provider.local_root())
        .and_then(|root| volume_free_bytes(&root));
    Ok(report)
}

/// Between jobs: apply the 2x rule to this free slot. Errors are swallowed
/// by the caller; this returns them so tests can see them.
///
/// `pool_default` is the pool's `build_baseline_gib` in bytes, used for a slot
/// that carries no recorded baseline; absent, the 4 GiB default applies.
///
/// # Errors
/// Returns an error if a build directory cannot be emptied.
pub fn after_slot_freed(
    dir: &Path,
    configured: Option<&str>,
    pool_default: Option<u64>,
) -> Result<CleanReport, VivariumError> {
    let (actions, skips) =
        clean_slot::reclaim_builds("-", "-", dir, configured, pool_default, false)?;
    Ok(CleanReport {
        actions,
        skips,
        ..CleanReport::default()
    })
}

/// Reclaim free slots when the volume is under [`DISK_RECLAIM_GIB`], then
/// refuse dispatch if it is still under [`DISK_REFUSE_GIB`].
///
/// # Errors
/// Returns an error when free space stays below the refuse floor.
pub fn guard_before_dispatch(space: &Mailspace, pool: &str) -> Result<(), VivariumError> {
    let handle = pools::open_one(space, Some(pool))?;
    if handle.config.provider != "local" {
        return Ok(());
    }
    let root = handle.provider.local_root().ok_or_else(|| {
        VivariumError::Message(format!("pool {} has no local root", handle.id.pool))
    })?;
    let Some(free) = volume_free_bytes(&root) else {
        return Ok(());
    };
    if free >= DISK_RECLAIM_GIB * GIB {
        return Ok(());
    }
    if let Err(error) = clean_pool(&handle, space, None, false) {
        eprintln!("warning: pool clean before dispatch failed: {error}");
    }
    let left = volume_free_bytes(&root).unwrap_or(free);
    if left < DISK_REFUSE_GIB * GIB {
        return Err(VivariumError::Message(format!(
            "refusing to dispatch on pool {pool}: {} free (need {} GiB); ran pool clean",
            format_bytes(left),
            DISK_REFUSE_GIB
        )));
    }
    Ok(())
}

/// Write `marks/build_baseline` on each free local slot after a seed.
///
/// # Errors
/// Returns an error if a mark cannot be written.
pub fn record_pool_baselines(handle: &PoolHandle) -> Result<(), VivariumError> {
    if handle.config.provider != "local" {
        return Ok(());
    }
    let root = handle.provider.local_root().ok_or_else(|| {
        VivariumError::Message(format!("pool {} has no local root", handle.id.pool))
    })?;
    for machine in handle.provider.list()? {
        let dir = root.join(&machine.id);
        if handle.id.owns(&machine) && slot_is_free(&dir) {
            clean_slot::record_seed_baseline(&dir, handle.config.build_dir.as_deref())?;
        }
    }
    Ok(())
}

fn clean_pool(
    handle: &PoolHandle,
    space: &Mailspace,
    base: Option<&str>,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    if handle.config.provider != "local" {
        return Ok(reset_remote_requested(handle, space, base, dry_run));
    }
    clean_local_pool(handle, dry_run)
}

fn clean_local_pool(
    handle: &PoolHandle,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    let root = local_root(handle)?;
    let pool = handle.id.pool.as_str();
    let configured = handle.config.build_dir.as_deref();
    let mut actions = Vec::new();
    let mut skips = Vec::new();
    for machine in handle.provider.list()? {
        if !handle.id.owns(&machine) {
            continue;
        }
        let dir = root.join(&machine.id);
        let (a, s) = clean_slot::reclaim_builds(
            pool,
            &machine.name,
            &dir,
            configured,
            handle.config.build_baseline_bytes(),
            dry_run,
        )?;
        actions.extend(a);
        skips.extend(s);
    }
    Ok((actions, skips))
}

fn local_root(handle: &PoolHandle) -> Result<PathBuf, VivariumError> {
    handle
        .provider
        .local_root()
        .ok_or_else(|| VivariumError::Message(format!("pool {} has no local root", handle.id.pool)))
}

/// `--base` on a remote pool: fetch+checkout that origin branch on every free slot.
fn reset_remote_requested(
    handle: &PoolHandle,
    space: &Mailspace,
    base: Option<&str>,
    dry_run: bool,
) -> (Vec<CleanAction>, Vec<CleanSkip>) {
    let Some(requested) = base else {
        return (Vec::new(), Vec::new());
    };
    let chosen = super::reset::choose_base(space, Some(requested));
    let Ok(machines) = handle.provider.list() else {
        return (Vec::new(), Vec::new());
    };
    let mut actions = Vec::new();
    let mut skips = Vec::new();
    for slot in super::slot::pool_slots(&handle.id, &machines)
        .into_iter()
        .filter(|s| s.state == "free")
    {
        reset_one_remote(handle, &slot, &chosen, dry_run, &mut actions, &mut skips);
    }
    (actions, skips)
}

fn reset_one_remote(
    handle: &PoolHandle,
    slot: &super::slot::SlotStatus,
    chosen: &super::reset::ChosenBase,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) {
    if dry_run {
        actions.push(CleanAction {
            pool: handle.id.pool.clone(),
            slot: slot.name.clone(),
            kind: ActionKind::Reset,
            bytes: 0,
            path: format!("origin/{}", chosen.name),
            note: chosen.why.into(),
        });
        return;
    }
    match super::reset::reset_slot(
        &handle.config,
        handle.provider.as_ref(),
        &slot.id,
        &chosen.name,
        chosen.why,
    ) {
        Ok(out) => actions.push(super::reset::reset_action(
            &handle.id.pool,
            &slot.name,
            &out,
        )),
        Err(e) => skips.push(super::reset::reset_skip(
            &handle.id.pool,
            &slot.name,
            &e.to_string(),
        )),
    }
}

#[cfg(test)]
#[path = "clean_test.rs"]
mod tests;
