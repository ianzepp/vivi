//! Slot identity, discovery, and planning. Everything is derived from the
//! provider's machine records; nothing is kept locally.

use std::collections::BTreeMap;

use serde::Serialize;

use super::provider::MachineRecord;

/// Metadata key marking a machine as a member of a pool (value: pool name).
pub const META_POOL: &str = "vivi_pool";
/// Metadata key holding the slot's 1-based index.
pub const META_SLOT: &str = "vivi_pool_slot";
/// Metadata key holding the base commit the slot was seeded at.
pub const META_BASE: &str = "vivi_pool_base";

/// Machine name for slot `index` of pool `pool`.
#[must_use]
pub fn slot_name(pool: &str, index: u32) -> String {
    format!("{pool}-slot-{index}")
}

/// Metadata written onto a new slot.
#[must_use]
pub fn slot_metadata(pool: &str, index: u32, base: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (META_POOL.to_string(), pool.to_string()),
        (META_SLOT.to_string(), index.to_string()),
        (META_BASE.to_string(), base.to_string()),
    ])
}

/// A pool member as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotStatus {
    pub name: String,
    pub id: String,
    /// `free` (stopped), `busy` (started), or the provider's own word.
    pub state: String,
    pub provider_state: String,
    pub index: Option<u32>,
    pub base: Option<String>,
    pub created_at: String,
    pub region: String,
}

fn slot_index(machine: &MachineRecord) -> Option<u32> {
    let raw = machine.metadata.get(META_SLOT)?;
    raw.parse::<u32>().into_iter().next()
}

/// Plain-language state for a provider state word.
#[must_use]
pub fn slot_state(provider_state: &str) -> &str {
    match provider_state {
        "stopped" => "free",
        "started" => "busy",
        other => other,
    }
}

/// The members of `pool` among `machines`, ordered by slot index then name.
#[must_use]
pub fn pool_slots(pool: &str, machines: &[MachineRecord]) -> Vec<SlotStatus> {
    let mut slots: Vec<SlotStatus> = machines
        .iter()
        .filter(|m| m.metadata.get(META_POOL).is_some_and(|p| p == pool))
        .map(|m| SlotStatus {
            name: m.name.clone(),
            id: m.id.clone(),
            state: slot_state(&m.state).to_string(),
            provider_state: m.state.clone(),
            index: slot_index(m),
            base: m.metadata.get(META_BASE).cloned(),
            created_at: m.created_at.clone(),
            region: m.region.clone(),
        })
        .collect();
    slots.sort_by(|a, b| {
        a.index
            .unwrap_or(u32::MAX)
            .cmp(&b.index.unwrap_or(u32::MAX))
            .then_with(|| a.name.cmp(&b.name))
    });
    slots
}

/// Indices to create so the pool reaches `target` slots: the lowest indices
/// not already taken. Empty when the pool already holds `target` or more.
#[must_use]
pub fn missing_indices(existing: &[SlotStatus], target: u32) -> Vec<u32> {
    let have = u32::try_from(existing.len()).unwrap_or(u32::MAX);
    let need = target.saturating_sub(have);
    let taken: Vec<u32> = existing.iter().filter_map(|s| s.index).collect();
    (1..=u32::MAX)
        .filter(|i| !taken.contains(i))
        .take(need as usize)
        .collect()
}

/// Find a slot by exact name or machine id.
#[must_use]
pub fn find_slot<'a>(slots: &'a [SlotStatus], selector: &str) -> Option<&'a SlotStatus> {
    slots
        .iter()
        .find(|s| s.name == selector || s.id == selector)
}

#[cfg(test)]
#[path = "slot_test.rs"]
mod tests;
