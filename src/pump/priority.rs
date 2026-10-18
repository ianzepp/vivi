//! Per-need dispatch priority, stored in `pump.db` (`need_priority`).
//!
//! The operator sets a level per need with `vivi pump priority`; the running
//! pump reads the current levels each tick through [`levels`]. Priority is not
//! part of the pump `State`, so a state save never overwrites it. A higher
//! level is more urgent; level 0 is no priority and has no row.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::VivariumError;

use super::control::project_home;
use super::host::Ctx;
use super::store::{DB_FILENAME, PumpStore};

/// The retired text file whose lines [`import_legacy`] turns into level 1.
const LEGACY_FILE: &str = "priority.txt";

/// Set `need` to `level`; level 0 removes it.
///
/// # Errors
/// Returns [`VivariumError`] for a negative level or when the database
/// cannot be opened or written.
pub fn set(home: &Path, need: &str, level: i64) -> Result<(), VivariumError> {
    if level < 0 {
        return Err(VivariumError::Message(format!(
            "priority level must be 0 or higher, got {level}"
        )));
    }
    let mut store = PumpStore::open(&home.join(DB_FILENAME))?;
    store.set_need_priority(need, level, now())
}

/// Remove any priority from `need`.
///
/// # Errors
/// Returns [`VivariumError`] when the database cannot be opened or written.
pub fn clear(home: &Path, need: &str) -> Result<(), VivariumError> {
    let mut store = PumpStore::open(&home.join(DB_FILENAME))?;
    store.clear_need_priority(need)
}

/// Every prioritized need, highest level first.
///
/// # Errors
/// Returns [`VivariumError`] when the database cannot be opened or read.
pub fn list(home: &Path) -> Result<Vec<(String, i64)>, VivariumError> {
    let store = PumpStore::open(&home.join(DB_FILENAME))?;
    store.need_priorities()
}

/// The current `need -> level` map, read fresh from `pump.db`. A missing
/// database, a database from before the table existed, or any read failure
/// gives an empty map: ranking falls back to plan order.
#[must_use]
pub fn levels(ctx: &Ctx) -> BTreeMap<String, i64> {
    let path = ctx.home.join(DB_FILENAME);
    let Ok(Some(store)) = PumpStore::open_read_only(&path) else {
        return BTreeMap::new();
    };
    store
        .need_priorities()
        .map(|rows| rows.into_iter().filter(|(_, level)| *level > 0).collect())
        .unwrap_or_default()
}

/// One-time import of the retired `<home>/priority.txt`: each non-comment
/// line becomes level 1 unless that need already has a level, then the file
/// is renamed to `priority.txt.imported`. Called by the pump at startup while
/// it holds the pump lock. A failure is logged and never stops the pump.
pub fn import_legacy(ctx: &Ctx) {
    let path = ctx.home.join(LEGACY_FILE);
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    match import_text(&ctx.home, &text) {
        Ok(count) => {
            let done = ctx.home.join(format!("{LEGACY_FILE}.imported"));
            match fs::rename(&path, &done) {
                Ok(()) => ctx.log(&format!(
                    "priority: imported {count} need(s) from {LEGACY_FILE} at level 1; renamed to {}",
                    done.display()
                )),
                Err(error) => ctx.log(&format!(
                    "priority: imported {LEGACY_FILE} but could not rename it: {error}"
                )),
            }
        }
        Err(error) => ctx.log(&format!(
            "priority: could not import {LEGACY_FILE}: {error}"
        )),
    }
}

/// Import the lines of a legacy file; returns how many needs gained a level.
fn import_text(home: &Path, text: &str) -> Result<usize, VivariumError> {
    let mut store = PumpStore::open_for_run(&home.join(DB_FILENAME))?;
    let have: BTreeSet<String> = store
        .need_priorities()?
        .into_iter()
        .map(|(need, _)| need)
        .collect();
    let mut added = 0;
    for need in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        if !have.contains(need) {
            store.set_need_priority(need, 1, now())?;
            added += 1;
        }
    }
    Ok(added)
}

/// `vivi pump priority set`.
///
/// # Errors
/// Returns [`VivariumError`] when the pump home cannot be resolved or written.
pub fn run_set(project: &Path, need: &str, level: i64) -> Result<(), VivariumError> {
    set(&project_home(project)?, need, level)?;
    if level == 0 {
        println!("priority cleared for {need}");
    } else {
        println!("priority {need} {level}");
    }
    Ok(())
}

/// `vivi pump priority clear`.
///
/// # Errors
/// Returns [`VivariumError`] when the pump home cannot be resolved or written.
pub fn run_clear(project: &Path, need: &str) -> Result<(), VivariumError> {
    clear(&project_home(project)?, need)?;
    println!("priority cleared for {need}");
    Ok(())
}

/// `vivi pump priority list`: `need level` lines, highest first.
///
/// # Errors
/// Returns [`VivariumError`] when the pump home cannot be resolved or read.
pub fn run_list(project: &Path) -> Result<(), VivariumError> {
    for (need, level) in list(&project_home(project)?)? {
        println!("{need} {level}");
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
