//! The slots page's data.
//!
//! The console cannot see a slot's files. A slot it knows by name is one the
//! config declares with a `[[slot]]` block, or one it has a record of: busy
//! when it has an open run, otherwise free. Slots a pool only counts
//! (`[pools.*].slots`) get a name once they have run something in the last
//! week; until then they are counted, not listed.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;
use vivi_error::VivariumError;

use super::config::{self, DeclaredPool, DeclaredSlot};
use super::runs::{self, KnownSlot, RunRow};
use super::{RunsState, with_runs};

/// How far back a slot must have run to be listed as free.
pub const KNOWN_WINDOW_SECS: i64 = 7 * 24 * 3600;
/// Recent runs read to name the slots.
pub const KNOWN_LIMIT: i64 = 5000;
/// Open runs read.
pub const OPEN_LIMIT: i64 = 1000;
/// Runs listed on a slot's history page.
pub const HISTORY_ROWS: i64 = 100;

/// A slot's last finished work, shown when it is free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRun {
    pub outcome: Option<String>,
    pub collected_at: Option<i64>,
}

/// Busy with a run, or free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    Busy(RunRow),
    Free(Option<LastRun>),
}

/// One slot the console knows by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotView {
    pub pool: String,
    pub slot: String,
    pub machine: String,
    pub state: SlotState,
}

/// Declared slots of a pool that have no recorded run, so no name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnseenSlots {
    pub pool: String,
    pub count: u32,
}

/// Everything the slots page shows for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotsData {
    pub now: i64,
    pub slots: Vec<SlotView>,
    pub unseen: Vec<UnseenSlots>,
}

/// Read the slots of the project at `root`, as of `now` (epoch seconds).
///
/// # Errors
/// Returns an error when the config or the store cannot be read.
pub fn load(root: &Path, now: i64) -> Result<RunsState<SlotsData>, VivariumError> {
    let config = config::load(root)?;
    let declared = config::declared_pools(&config);
    let named = config::declared_slots(&config);
    with_runs(root, |conn| slots_data(conn, now, &declared, &named))
}

fn slots_data(
    conn: &Connection,
    now: i64,
    declared: &[DeclaredPool],
    named: &[DeclaredSlot],
) -> Result<SlotsData, VivariumError> {
    let known = runs::known_slots(conn, now - KNOWN_WINDOW_SECS, KNOWN_LIMIT)?;
    let open = runs::open_runs(conn, OPEN_LIMIT)?;
    let slots = add_declared(merge(known, open), named);
    let unseen = unseen_slots(declared, &slots);
    Ok(SlotsData { now, slots, unseen })
}

/// Join recent history with open runs. An open run wins over history; when
/// a slot has several open runs the newest is the busy one.
fn merge(known: Vec<KnownSlot>, open: Vec<RunRow>) -> Vec<SlotView> {
    let mut by_slot: BTreeMap<(String, String), SlotView> = BTreeMap::new();
    for slot in known {
        let last = LastRun {
            outcome: slot.last_outcome,
            collected_at: slot.last_collected_at,
        };
        by_slot.insert(
            (slot.pool.clone(), slot.slot.clone()),
            SlotView {
                pool: slot.pool,
                slot: slot.slot,
                machine: slot.machine,
                state: SlotState::Free(Some(last)),
            },
        );
    }
    // `open` is newest first, so the first open run seen for a slot wins.
    let mut taken: Vec<(String, String)> = Vec::new();
    for run in open {
        let key = (run.pool.clone(), run.slot.clone());
        if taken.contains(&key) {
            continue;
        }
        taken.push(key.clone());
        by_slot.insert(
            key,
            SlotView {
                pool: run.pool.clone(),
                slot: run.slot.clone(),
                machine: run.machine.clone(),
                state: SlotState::Busy(run),
            },
        );
    }
    let mut slots: Vec<SlotView> = by_slot.into_values().collect();
    sort_slots(&mut slots);
    slots
}

fn sort_slots(slots: &mut [SlotView]) {
    slots.sort_by(|a, b| {
        (a.pool.as_str(), natural_key(&a.slot)).cmp(&(b.pool.as_str(), natural_key(&b.slot)))
    });
}

/// Add every `[[slot]]`-declared slot the history did not name, as a free
/// slot with no last run, once per pool it serves. A declared slot that
/// already has a record keeps it.
fn add_declared(mut slots: Vec<SlotView>, named: &[DeclaredSlot]) -> Vec<SlotView> {
    for slot in named {
        for pool in &slot.pools {
            let known = slots
                .iter()
                .any(|view| view.pool == *pool && view.slot == slot.name);
            if !known {
                slots.push(SlotView {
                    pool: pool.clone(),
                    slot: slot.name.clone(),
                    machine: slot.host.clone(),
                    state: SlotState::Free(None),
                });
            }
        }
    }
    sort_slots(&mut slots);
    slots
}

/// A sort key that puts `slot-2` before `slot-10`: the text before the last
/// run of digits, then the number.
fn natural_key(name: &str) -> (String, u64, String) {
    let digits = name.chars().rev().take_while(char::is_ascii_digit).count();
    let split = name.len() - digits;
    let number = name[split..].parse().unwrap_or(0);
    (name[..split].to_string(), number, name.to_string())
}

fn unseen_slots(declared: &[DeclaredPool], slots: &[SlotView]) -> Vec<UnseenSlots> {
    declared
        .iter()
        .filter_map(|pool| {
            let seen = slots.iter().filter(|slot| slot.pool == pool.name).count();
            let missing = u32::try_from(seen).map_or(0, |seen| pool.slots.saturating_sub(seen));
            (missing > 0).then(|| UnseenSlots {
                pool: pool.name.clone(),
                count: missing,
            })
        })
        .collect()
}

/// Read one slot's recent runs, newest first.
///
/// # Errors
/// Returns an error when the store cannot be read.
pub fn history(
    root: &Path,
    pool: &str,
    slot: &str,
) -> Result<RunsState<Vec<RunRow>>, VivariumError> {
    with_runs(root, |conn| {
        runs::slot_history(conn, pool, slot, HISTORY_ROWS)
    })
}

#[cfg(test)]
#[path = "slots_test.rs"]
mod tests;
