//! Per-slot build-output reclaim under the 2x rule.

use std::path::{Path, PathBuf};

use super::clean::{ActionKind, CleanAction, CleanSkip};
use super::clean_rule::{
    build_paths, dir_bytes, effective_baseline, empty_dir, over_cap, read_baseline, write_baseline,
};
use super::local_proc;
use super::local_slots::{CHECKOUT, RECLAIM_FILE, hold_reclaim, is_known, release_reclaim};
use super::slot::STATE_READY;
use crate::error::VivariumError;

/// True when the slot is claimed, its boot process is still alive, or a
/// build-tree reclaim is emptying it.
#[must_use]
pub fn slot_is_busy(dir: &Path) -> bool {
    dir.join("claim").is_file() || dir.join(RECLAIM_FILE).is_file() || local_proc::is_alive(dir)
}

/// True when the slot is seeded, stopped, and unclaimed.
#[must_use]
pub fn slot_is_free(dir: &Path) -> bool {
    is_known(dir)
        && !slot_is_busy(dir)
        && super::clean_rule::read_mark(dir, "vivi_pool_state").as_deref() == Some(STATE_READY)
}

/// Checkouts that may hold build output: the slot `repo/` and each
/// `targets/<hash>/repo`.
#[must_use]
pub fn checkouts(slot: &Path) -> Vec<PathBuf> {
    let mut out = vec![slot.join(CHECKOUT)];
    let targets = slot.join("targets");
    let Ok(entries) = std::fs::read_dir(&targets) else {
        return out.into_iter().filter(|p| p.is_dir()).collect();
    };
    for entry in entries.flatten() {
        let repo = entry.path().join(CHECKOUT);
        if repo.is_dir() {
            out.push(repo);
        }
    }
    out.into_iter().filter(|p| p.is_dir()).collect()
}

/// Reclaim each over-cap build directory of a free slot. Busy slots are
/// reported as skipped and never touched.
///
/// # Errors
/// Returns an error if a build directory cannot be emptied.
pub fn reclaim_builds(
    pool: &str,
    slot: &str,
    dir: &Path,
    configured: Option<&str>,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    if !slot_is_free(dir) {
        return Ok((
            Vec::new(),
            vec![skip(pool, slot, ActionKind::Build, "slot is mid-dispatch")],
        ));
    }
    if !hold_reclaim(dir)? {
        return Ok((
            Vec::new(),
            vec![skip(
                pool,
                slot,
                ActionKind::Build,
                "a reclaim is already emptying this slot",
            )],
        ));
    }
    let held = reclaim_builds_held(pool, slot, dir, configured, dry_run);
    release_reclaim(dir)?;
    held
}

/// The per-checkout loop under an already-planted reclaim hold. A claim that
/// landed between the free check and the hold still stops everything.
fn reclaim_builds_held(
    pool: &str,
    slot: &str,
    dir: &Path,
    configured: Option<&str>,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    if dir.join("claim").is_file() {
        return Ok((
            Vec::new(),
            vec![skip(
                pool,
                slot,
                ActionKind::Build,
                "claimed while reclaiming",
            )],
        ));
    }
    let mut actions = Vec::new();
    let mut skips = Vec::new();
    let baseline = effective_baseline(read_baseline(dir), None);
    for checkout in checkouts(dir) {
        for path in build_paths(&checkout, configured) {
            push_build(
                pool,
                slot,
                dir,
                &path,
                baseline,
                dry_run,
                &mut actions,
                &mut skips,
            )?;
        }
    }
    Ok((actions, skips))
}

#[allow(clippy::too_many_arguments)]
fn push_build(
    pool: &str,
    slot: &str,
    dir: &Path,
    path: &Path,
    baseline: u64,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    let bytes = dir_bytes(path);
    if !over_cap(bytes, baseline) {
        if read_baseline(dir).is_none() && bytes > 0 {
            write_baseline(dir, bytes)?;
        }
        return Ok(());
    }
    if dry_run {
        actions.push(action(
            pool,
            slot,
            ActionKind::Build,
            bytes,
            path,
            "over 2x baseline",
        ));
        return Ok(());
    }
    match empty_dir(path) {
        Ok(reclaimed) => {
            actions.push(action(
                pool,
                slot,
                ActionKind::Build,
                reclaimed,
                path,
                "reclaimed over 2x baseline",
            ));
            if read_baseline(dir).is_none() {
                write_baseline(dir, baseline)?;
            }
        }
        Err(e) => skips.push(skip(pool, slot, ActionKind::Build, &e.to_string())),
    }
    Ok(())
}

/// Measure the slot's build directories and write `marks/build_baseline`
/// when the size is non-zero and no mark exists yet.
///
/// # Errors
/// Returns an error if the mark cannot be written.
pub fn record_seed_baseline(dir: &Path, configured: Option<&str>) -> Result<(), VivariumError> {
    if read_baseline(dir).is_some() {
        return Ok(());
    }
    let bytes: u64 = checkouts(dir)
        .iter()
        .flat_map(|c| build_paths(c, configured))
        .map(|p| dir_bytes(&p))
        .sum();
    if bytes > 0 {
        write_baseline(dir, bytes)?;
    }
    Ok(())
}

fn action(
    pool: &str,
    slot: &str,
    kind: ActionKind,
    bytes: u64,
    path: &Path,
    note: &str,
) -> CleanAction {
    CleanAction {
        pool: pool.to_string(),
        slot: slot.to_string(),
        kind,
        bytes,
        path: path.display().to_string(),
        note: note.to_string(),
    }
}

fn skip(pool: &str, slot: &str, kind: ActionKind, reason: &str) -> CleanSkip {
    CleanSkip {
        pool: pool.to_string(),
        slot: slot.to_string(),
        kind,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
#[path = "clean_slot_test.rs"]
mod tests;
