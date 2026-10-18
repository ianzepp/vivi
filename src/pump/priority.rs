//! Per-need dispatch priority, stored in `pump.db` (`need_priority`).
//!
//! The operator sets a level per need with `vivi pump priority`; the running
//! pump reads the current levels each tick through [`levels`]. Priority is not
//! part of the pump `State`, so a state save never overwrites it. A higher
//! level is more urgent; level 0 is no priority and has no row.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::VivariumError;

use super::control::project_home;
use super::host::Ctx;
use super::store::{DB_FILENAME, PumpStore};

/// The retired text file whose lines [`import_legacy`] turns into level 1.
const LEGACY_FILE: &str = "priority.txt";

/// The levels last read for one pump home.
struct Cached {
    /// The `ctx.now()` second the map was read (or reused) in.
    second: u64,
    /// The last map read successfully; empty before any success.
    map: BTreeMap<String, i64>,
    /// The latest read failed, so `map` is the last good one.
    failing: bool,
}

/// Per-home level cache, so many callers in one tick open `pump.db` once.
fn lock_cache() -> MutexGuard<'static, HashMap<PathBuf, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Forget the cached levels for `home` so the next [`levels`] re-reads.
fn invalidate(home: &Path) {
    lock_cache().remove(home);
}

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
    let result = store.set_need_priority(need, level, now());
    invalidate(home);
    result
}

/// Remove any priority from `need`.
///
/// # Errors
/// Returns [`VivariumError`] when the database cannot be opened or written.
pub fn clear(home: &Path, need: &str) -> Result<(), VivariumError> {
    let mut store = PumpStore::open(&home.join(DB_FILENAME))?;
    let result = store.clear_need_priority(need);
    invalidate(home);
    result
}

/// Every prioritized need, highest level first.
///
/// # Errors
/// Returns [`VivariumError`] when the database cannot be opened or read.
pub fn list(home: &Path) -> Result<Vec<(String, i64)>, VivariumError> {
    let store = PumpStore::open(&home.join(DB_FILENAME))?;
    store.need_priorities()
}

/// The current `need -> level` map from `pump.db`, read at most once per
/// pump home per `ctx.now()` second; [`set`] and [`clear`] drop the cache. A
/// missing database is an empty map. When a read fails (a busy database, a
/// database from before the table existed) the last good map for that home is
/// returned, or an empty map when there is none, and the failure is logged
/// once per streak: ranking falls back to plan order only when no level was
/// ever read.
#[must_use]
pub fn levels(ctx: &Ctx) -> BTreeMap<String, i64> {
    let second = ctx.now();
    let mut cache = lock_cache();
    if let Some(entry) = cache.get(&ctx.home)
        && entry.second == second
    {
        return entry.map.clone();
    }
    let (previous, was_failing) = cache
        .remove(&ctx.home)
        .map_or((BTreeMap::new(), false), |entry| (entry.map, entry.failing));
    let entry = match read_levels(&ctx.home.join(DB_FILENAME)) {
        Ok(map) => Cached {
            second,
            map,
            failing: false,
        },
        Err(error) => {
            if !was_failing {
                ctx.log(&format!(
                    "priority: could not read levels, using the last good map: {error}"
                ));
            }
            Cached {
                second,
                map: previous,
                failing: true,
            }
        }
    };
    let map = entry.map.clone();
    cache.insert(ctx.home.clone(), entry);
    map
}

fn read_levels(path: &Path) -> Result<BTreeMap<String, i64>, VivariumError> {
    let Some(store) = PumpStore::open_read_only(path)? else {
        return Ok(BTreeMap::new());
    };
    Ok(store
        .need_priorities()?
        .into_iter()
        .filter(|(_, level)| *level > 0)
        .collect())
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
    invalidate(home);
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
    invalidate(home);
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
