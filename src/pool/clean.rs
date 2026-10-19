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
//! `targets/<hash>` caches dropped, and a fresh detached worktree restored
//! at origin/`[pump] main` (or `--base`). Recycle covers the unit's work
//! pool and the merge role's pool (merge-lane count is that pool's `slots`).
//! A dirty tree or commits not contained in the merge lane are
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
    Reset,
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
            options.base.as_deref(),
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
/// free **local** work lanes onto origin/`[pump] main`. Covers the unit's
/// work pool and the merge role's pool (merge-lane count is that pool's
/// `slots`). Remote (fly/docker/ssh) slots are left for the next
/// dispatch-time reset — pump does not persist which remote slot last
/// served the unit after unclaim. No mailspace or no local pool is a
/// silent no-op.
///
/// # Errors
/// Returns an error if a matching free slot cannot be recycled safely.
pub fn after_merged(project: &Path, unit: &Unit) -> Result<CleanReport, VivariumError> {
    let Ok(space) = Mailspace::discover(Some(project)) else {
        return Ok(CleanReport::default());
    };
    let st = load_state(&space);
    let mut report = CleanReport::default();
    for selector in recycle_selectors(&space, unit) {
        take_local_pools(
            &space,
            selector.as_deref(),
            st.as_ref(),
            Some(unit),
            &mut report,
        )?;
    }
    report.take(clean_pump::reclaim_lanes(&space, st.as_ref(), false)?);
    Ok(report)
}

/// The unit's work pool, plus the merge role's pool when that is a
/// different assignment. `None` means every pool (legacy units with no
/// `pool`).
fn recycle_selectors(space: &Mailspace, unit: &Unit) -> Vec<Option<String>> {
    let Some(hand) = unit.pool.as_deref() else {
        return vec![None];
    };
    let mut names = vec![Some(hand.to_string())];
    if let Some(merge) = crate::routing::from_mailspace(&space.config)
        .merge()
        .and_then(|role| role.pool.clone())
        && merge != hand
    {
        names.push(Some(merge));
    }
    names
}

fn take_local_pools(
    space: &Mailspace,
    pool: Option<&str>,
    st: Option<&State>,
    hint: Option<&Unit>,
    report: &mut CleanReport,
) -> Result<(), VivariumError> {
    let Ok(handles) = pools::open(space, pool, Need::Any) else {
        return Ok(());
    };
    for handle in &handles {
        if handle.config.provider != "local" {
            continue;
        }
        report.take(clean_pool(handle, space, st, hint, None, false)?);
    }
    Ok(())
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
    let root = expand_root(handle.config.local.root_or_default());
    let Some(free) = volume_free_bytes(&root) else {
        return Ok(());
    };
    if free >= DISK_RECLAIM_GIB * GIB {
        return Ok(());
    }
    let st = load_state(space);
    let _ignored = clean_pool(&handle, space, st.as_ref(), None, None, false);
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
    base: Option<&str>,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    if handle.config.provider != "local" {
        return Ok(reset_remote_requested(handle, space, base, dry_run));
    }
    let root = expand_root(handle.config.local.root_or_default());
    let pool = handle.id.pool.as_str();
    // The local provider lists every slot under the shared root, across all
    // mailspaces and pools; touch only this pool's own
    // `<mailspace>/<pool>/` slots (trailing slash, so pool `here` cannot
    // match pool `here2`).
    let prefix = format!("{}/{}/", handle.id.mailspace, handle.id.pool);
    let configured = handle.config.build_dir.as_deref();
    let mut actions = Vec::new();
    let mut skips = Vec::new();
    for machine in handle.provider.list()? {
        if !machine.id.starts_with(&prefix) {
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
        if lane_is_done(st, hint, &dir) {
            recycle(
                handle,
                space,
                st,
                hint,
                &dir,
                &machine.name,
                base,
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
    base: Option<&str>,
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
    let chosen = super::reset::choose_base(space, base, hint, st, super::reset::ResetKind::Recycle);
    let (a, s) = clean_work::recycle_lane(
        &handle.id.pool,
        name,
        dir,
        &lane,
        handle.config.build_dir.as_deref(),
        &chosen.name,
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
    let pool_root = clean_work::real_path(&root.join(&handle.id.mailspace).join(&handle.id.pool));
    let (a, s) = clean_work::sweep_orphans(
        &handle.id.pool,
        &source,
        root,
        lane.as_deref(),
        |path| {
            // A worktree outside this pool's own `<mailspace>/<pool>`
            // prefix belongs to another mailspace sharing the root; leave
            // it alone.
            clean_work::real_path(path).starts_with(&pool_root) && orphan_is_done(st, &prefix, path)
        },
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
    let chosen = super::reset::choose_base(
        space,
        Some(requested),
        None,
        None,
        super::reset::ResetKind::Recycle,
    );
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

fn slot_dir_under(root: &Path, path: &Path) -> Option<std::path::PathBuf> {
    let root = super::clean_work::real_path(root);
    let path = super::clean_work::real_path(path);
    let rel = match path.strip_prefix(&root) {
        Ok(rel) => rel,
        Err(_) => return None,
    };
    let mut cur = root;
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
