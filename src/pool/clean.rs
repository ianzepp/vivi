//! Reclaim disposable pool build output and recycle finished work lanes.
//!
//! **2x rule.** A slot's build directory (configured `build_dir`, plus Cargo
//! `target/`) is kept warm between jobs. When its size exceeds twice the
//! post-fresh-build baseline (`marks/build_baseline`, or 4 GiB when the mark
//! is missing) the contents are deleted. The next job rebuilds cold.
//!
//! **Work-lane recycle.** When pump marks a unit `merged` — that is merge-lane
//! success, not landing on main — a free slot that last served that unit has
//! its worktree removed (`git worktree remove`, then prune), its
//! `targets/<hash>` caches dropped, and a fresh detached worktree restored at
//! the seed base. A dirty tree or commits not contained in the merge lane are
//! reported, never deleted.
//!
//! Never touches a slot that is claimed or whose boot process is alive.

use std::path::Path;

use super::clean_pump::{self, is_done, load_state, merge_lane_ref, unit_for};
use super::clean_rule::{
    DISK_RECLAIM_GIB, DISK_REFUSE_GIB, GIB, MARK_LAST_BRANCH, MARK_LAST_TASK, format_bytes,
    read_mark, volume_free_bytes,
};
use super::clean_slot::{self, slot_is_free};
use super::clean_work;
use super::local_slots::expand_root;
use super::pools::{self, Need, PoolHandle};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::state::{State, Unit};

pub mod rule {
    pub use super::super::clean_rule::*;
}

/// Kind of reclaimable residue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Build,
    Worktree,
    Target,
    Branch,
    Orphan,
    LaneBuild,
}

impl ActionKind {
    /// Porcelain token.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Worktree => "worktree",
            Self::Target => "target",
            Self::Branch => "branch",
            Self::Orphan => "orphan",
            Self::LaneBuild => "lane-build",
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
}

/// Reclaim free slots of every selected local pool, then pump merge lanes.
///
/// # Errors
/// Returns an error if a pool cannot be opened or a reclaim fails hard.
pub fn clean_mailspace(
    space: &Mailspace,
    options: &CleanOptions,
) -> Result<CleanReport, VivariumError> {
    let handles = pools::open(space, options.pool.as_deref(), Need::Any)?;
    let st = load_state(space);
    let mut report = CleanReport::default();
    for handle in &handles {
        report.take(clean_pool(
            handle,
            space,
            st.as_ref(),
            None,
            options.dry_run,
        )?);
    }
    if options.pool.is_none() {
        report.take(clean_pump::reclaim_lanes(
            space,
            st.as_ref(),
            options.dry_run,
        )?);
    }
    report.free_bytes = handles
        .iter()
        .find(|h| h.config.provider == "local")
        .and_then(|h| volume_free_bytes(&expand_root(h.config.local.root_or_default())));
    Ok(report)
}

/// After pump marks `unit` merged into its merge lane: recycle matching
/// free work lanes. No mailspace or no local pool is a silent no-op.
///
/// # Errors
/// Returns an error if a matching free slot cannot be recycled safely.
pub fn after_merged(project: &Path, unit: &Unit) -> Result<CleanReport, VivariumError> {
    let Ok(space) = Mailspace::discover(Some(project)) else {
        return Ok(CleanReport::default());
    };
    let Ok(handles) = pools::open(&space, unit.pool.as_deref(), Need::Any) else {
        return Ok(CleanReport::default());
    };
    let st = load_state(&space);
    let mut report = CleanReport::default();
    for handle in &handles {
        if handle.config.provider != "local" {
            continue;
        }
        report.take(clean_pool(handle, &space, st.as_ref(), Some(unit), false)?);
    }
    report.take(clean_pump::reclaim_lanes(&space, st.as_ref(), false)?);
    Ok(report)
}

/// Between jobs: apply the 2x rule to this free slot. Errors are swallowed
/// by the caller; this returns them so tests can see them.
///
/// # Errors
/// Returns an error if a build directory cannot be emptied.
pub fn after_slot_freed(
    dir: &Path,
    configured: Option<&str>,
) -> Result<CleanReport, VivariumError> {
    let (actions, skips) = clean_slot::reclaim_builds("-", "-", dir, configured, false)?;
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
    let root = expand_root(handle.config.local.root_or_default());
    let Some(free) = volume_free_bytes(&root) else {
        return Ok(());
    };
    if free >= DISK_RECLAIM_GIB * GIB {
        return Ok(());
    }
    let st = load_state(space);
    let _ignored = clean_pool(&handle, space, st.as_ref(), None, false);
    let _ignored = clean_pump::reclaim_lanes(space, st.as_ref(), false);
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
    let root = expand_root(handle.config.local.root_or_default());
    let prefix = format!("{}/{}", handle.id.mailspace, handle.id.pool);
    for machine in handle.provider.list()? {
        let dir = root.join(&machine.id);
        if machine.id.starts_with(&prefix) && slot_is_free(&dir) {
            clean_slot::record_seed_baseline(&dir, handle.config.build_dir.as_deref())?;
        }
    }
    Ok(())
}

fn clean_pool(
    handle: &PoolHandle,
    space: &Mailspace,
    st: Option<&State>,
    hint: Option<&Unit>,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    if handle.config.provider != "local" {
        return Ok((Vec::new(), Vec::new()));
    }
    let root = expand_root(handle.config.local.root_or_default());
    let pool = handle.id.pool.as_str();
    let configured = handle.config.build_dir.as_deref();
    let mut actions = Vec::new();
    let mut skips = Vec::new();
    for machine in handle.provider.list()? {
        let dir = root.join(&machine.id);
        let (a, s) = clean_slot::reclaim_builds(pool, &machine.name, &dir, configured, dry_run)?;
        actions.extend(a);
        skips.extend(s);
        if lane_is_done(st, hint, &dir) {
            recycle(
                handle,
                space,
                st,
                hint,
                &dir,
                &machine.name,
                dry_run,
                &mut actions,
                &mut skips,
            )?;
        }
    }
    sweep(handle, space, st, &root, dry_run, &mut actions, &mut skips)?;
    Ok((actions, skips))
}

fn lane_is_done(st: Option<&State>, hint: Option<&Unit>, dir: &Path) -> bool {
    let task = read_mark(dir, MARK_LAST_TASK);
    let branch = read_mark(dir, MARK_LAST_BRANCH);
    if let Some(unit) = hint {
        let hit = task.is_some() && task.as_deref() == unit.task.as_deref()
            || branch.is_some() && branch.as_deref() == unit.branch.as_deref();
        if hit {
            return is_done(unit.status);
        }
    }
    st.and_then(|s| unit_for(s, task.as_deref(), branch.as_deref()))
        .is_some_and(|unit| is_done(unit.status))
}

#[allow(clippy::too_many_arguments)]
fn recycle(
    handle: &PoolHandle,
    space: &Mailspace,
    st: Option<&State>,
    hint: Option<&Unit>,
    dir: &Path,
    name: &str,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    let target = hint
        .map(|u| u.target_id().to_string())
        .or_else(|| {
            let task = read_mark(dir, MARK_LAST_TASK);
            let branch = read_mark(dir, MARK_LAST_BRANCH);
            st.and_then(|s| unit_for(s, task.as_deref(), branch.as_deref()))
                .map(|u: &Unit| u.target_id().to_string())
        })
        .unwrap_or_else(|| "default".into());
    let Some(lane) = merge_lane_ref(space, &target) else {
        skips.push(CleanSkip {
            pool: handle.id.pool.clone(),
            slot: name.into(),
            kind: ActionKind::Worktree,
            reason: "no merge-lane ref in [pump]".into(),
        });
        return Ok(());
    };
    let (a, s) = clean_work::recycle_lane(
        &handle.id.pool,
        name,
        dir,
        &lane,
        handle.config.build_dir.as_deref(),
        dry_run,
    )?;
    actions.extend(a);
    skips.extend(s);
    Ok(())
}

fn sweep(
    handle: &PoolHandle,
    space: &Mailspace,
    st: Option<&State>,
    root: &Path,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    let Some(source) = handle.config.local.worktree_path() else {
        return Ok(());
    };
    let lane = merge_lane_ref(space, "default");
    let prefix = root.to_path_buf();
    let (a, s) = clean_work::sweep_orphans(
        &handle.id.pool,
        &source,
        root,
        lane.as_deref(),
        |path| orphan_is_done(st, &prefix, path),
        dry_run,
    )?;
    actions.extend(a);
    skips.extend(s);
    Ok(())
}

fn orphan_is_done(st: Option<&State>, root: &Path, path: &Path) -> bool {
    let Some(dir) = slot_dir_under(root, path) else {
        return true;
    };
    if !slot_is_free(&dir) {
        return false;
    }
    let task = read_mark(&dir, MARK_LAST_TASK);
    let branch = read_mark(&dir, MARK_LAST_BRANCH);
    st.is_some_and(|s| {
        unit_for(s, task.as_deref(), branch.as_deref()).is_some_and(|u| is_done(u.status))
    })
}

fn slot_dir_under(root: &Path, path: &Path) -> Option<std::path::PathBuf> {
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return None,
    };
    let mut cur = root.to_path_buf();
    for part in rel {
        cur.push(part);
        if part.to_string_lossy().starts_with("slot-") {
            return Some(cur);
        }
    }
    None
}

#[cfg(test)]
#[path = "clean_test.rs"]
mod tests;
