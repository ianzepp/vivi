//! Pump state used by pool clean: done units and merge-lane build dirs.

use std::path::Path;

use super::clean::{ActionKind, CleanAction, CleanSkip};
use super::clean_rule::{
    build_paths, dir_bytes, effective_baseline, empty_dir, over_cap, read_baseline, write_baseline,
};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::state::{State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Load live pump units from `pump.db`, else `state.json`. Missing is `None`.
#[must_use]
pub fn load_state(space: &Mailspace) -> Option<State> {
    let pump = space.config.pump.as_ref()?;
    let home = pump.home_dir(&space.root);
    let db = home.join(DB_FILENAME);
    if let Ok(Some(store)) = PumpStore::open_read_only(&db)
        && let Ok(state) = store.load()
    {
        return Some(state);
    }
    match State::load(&home.join("state.json")) {
        Ok(state) => Some(state),
        Err(_) => None,
    }
}

/// Merge-lane success, or a unit that will never merge. Not landing on main.
#[must_use]
pub fn is_done(status: Status) -> bool {
    matches!(
        status,
        Status::Merged | Status::NoDefect | Status::Replaced | Status::Withdrawn
    )
}

/// The unit last bound to `task` or `branch`.
#[must_use]
pub fn unit_for<'a>(st: &'a State, task: Option<&str>, branch: Option<&str>) -> Option<&'a Unit> {
    st.units.values().find(|unit| {
        task.is_some() && unit.task.as_deref() == task
            || branch.is_some() && unit.branch.as_deref() == branch
    })
}

/// Main-branch name for `target`, from `[pump]` / `[pump.targets]`.
#[must_use]
pub fn main_ref(space: &Mailspace, target: &str) -> Option<String> {
    let pump = space.config.pump.as_ref()?;
    let named = pump
        .targets
        .get(target)
        .map(|t| t.main.clone())
        .filter(|b| !b.is_empty());
    named.or_else(|| {
        let main = pump.main.clone();
        (!main.is_empty()).then_some(main)
    })
}

/// True when any unit of `target` is inside a merge lane right now.
#[must_use]
pub fn target_is_merging(st: &State, target: &str) -> bool {
    st.units
        .values()
        .any(|unit| unit.target_id() == target && unit.status == Status::Merging)
}

/// Apply the 2x rule to pump merge-lane checkouts (`<home>/merge-lanes`
/// and the integration clone). A target that is mid-merge is skipped.
///
/// # Errors
/// Returns an error if a build directory cannot be emptied.
pub fn reclaim_lanes(
    space: &Mailspace,
    st: Option<&State>,
    dry_run: bool,
) -> Result<(Vec<CleanAction>, Vec<CleanSkip>), VivariumError> {
    let Some(pump) = space.config.pump.as_ref() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let home = pump.home_dir(&space.root);
    let mut actions = Vec::new();
    let mut skips = Vec::new();
    push_checkout(
        "pump",
        "lane-0",
        &pump.repo_dir(&space.root),
        st.is_some_and(|s| target_is_merging(s, "default")),
        dry_run,
        &mut actions,
        &mut skips,
    )?;
    reclaim_named_lanes(
        &home.join("merge-lanes"),
        st,
        dry_run,
        &mut actions,
        &mut skips,
    )?;
    Ok((actions, skips))
}

fn reclaim_named_lanes(
    root: &Path,
    st: Option<&State>,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    let Ok(targets) = std::fs::read_dir(root) else {
        return Ok(());
    };
    for target in targets.flatten() {
        let name = target.file_name().to_string_lossy().into_owned();
        let merging = st.is_some_and(|s| target_is_merging(s, &name));
        let Ok(lanes) = std::fs::read_dir(target.path()) else {
            continue;
        };
        for lane in lanes.flatten() {
            let slot = format!("{}-{}", name, lane.file_name().to_string_lossy());
            push_checkout(
                "pump",
                &slot,
                &lane.path(),
                merging,
                dry_run,
                actions,
                skips,
            )?;
        }
    }
    Ok(())
}

fn push_checkout(
    pool: &str,
    slot: &str,
    checkout: &Path,
    merging: bool,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    if merging {
        skips.push(CleanSkip {
            pool: pool.into(),
            slot: slot.into(),
            kind: ActionKind::LaneBuild,
            reason: "merge lane is mid-batch".into(),
        });
        return Ok(());
    }
    let baseline = effective_baseline(read_baseline(checkout), None);
    for path in build_paths(checkout, None) {
        reclaim_lane_build(
            pool, slot, checkout, &path, baseline, dry_run, actions, skips,
        )?;
    }
    Ok(())
}

fn reclaim_lane_build(
    pool: &str,
    slot: &str,
    checkout: &Path,
    path: &Path,
    baseline: u64,
    dry_run: bool,
    actions: &mut Vec<CleanAction>,
    skips: &mut Vec<CleanSkip>,
) -> Result<(), VivariumError> {
    let bytes = dir_bytes(path);
    if !over_cap(bytes, baseline) {
        if read_baseline(checkout).is_none() && bytes > 0 {
            write_baseline(checkout, bytes)?;
        }
        return Ok(());
    }
    if dry_run {
        actions.push(lane_act(pool, slot, bytes, path, "over 2x"));
        return Ok(());
    }
    match empty_dir(path) {
        Ok(reclaimed) => actions.push(lane_act(pool, slot, reclaimed, path, "reclaimed")),
        Err(e) => skips.push(CleanSkip {
            pool: pool.into(),
            slot: slot.into(),
            kind: ActionKind::LaneBuild,
            reason: e.to_string(),
        }),
    }
    Ok(())
}

fn lane_act(pool: &str, slot: &str, bytes: u64, path: &Path, note: &str) -> CleanAction {
    CleanAction {
        pool: pool.to_string(),
        slot: slot.to_string(),
        kind: ActionKind::LaneBuild,
        bytes,
        path: path.display().to_string(),
        note: note.to_string(),
    }
}

#[cfg(test)]
#[path = "clean_pump_test.rs"]
mod tests;
