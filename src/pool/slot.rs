//! Slot identity, discovery, and planning. Everything is derived from the
//! provider's machine records; nothing is kept locally.

use std::collections::BTreeMap;

use serde::Serialize;

use super::provider::MachineRecord;

/// Metadata key marking a machine as a member of a pool (value: pool name).
pub const META_POOL: &str = "vivi_pool";
/// Metadata key naming the mailspace the pool belongs to (value: the
/// mailspace's slug). A slot belongs to a mailspace AND a pool: two
/// mailspaces may each have a pool of the same name on one host.
pub const META_MAILSPACE: &str = "vivi_pool_mailspace";
/// Metadata key holding the slot's 1-based index.
pub const META_SLOT: &str = "vivi_pool_slot";
/// Metadata key holding the base commit the slot was seeded at.
pub const META_BASE: &str = "vivi_pool_base";
/// Metadata key holding the seed state: `seeding` until `create` has seen the
/// seed finish and cleared the fetch token, then `ready`.
pub const META_STATE: &str = "vivi_pool_state";
/// Metadata key holding the handle of the task that holds the slot. Set at
/// claim, removed when the result is collected. A slot with this key is never
/// claimed, whatever state the machine is in.
pub const META_TASK: &str = "vivi_pool_task";
/// Metadata key holding the claim time (epoch seconds).
pub const META_CLAIMED: &str = "vivi_pool_claimed";
/// Metadata key holding the machine's CPU architecture, when the host knows
/// it (`arm64`, `amd64`).
pub const META_ARCH: &str = "vivi_pool_arch";

pub const STATE_SEEDING: &str = "seeding";
pub const STATE_READY: &str = "ready";

/// Suffix of the self-test pool that belongs to a pool (`<pool>-doctor`).
pub const DOCTOR_SUFFIX: &str = "-doctor";

/// The identity of a pool: the mailspace it belongs to (as a slug) and its
/// own name (the key of its `[pools.<name>]` table). Every mark, machine name
/// and directory a pool makes derives from both.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PoolId {
    pub mailspace: String,
    pub pool: String,
}

impl PoolId {
    /// The pool `pool` of the mailspace named `mailspace_name` (the name is
    /// normalised by [`space_slug`]).
    #[must_use]
    pub fn new(mailspace_name: &str, pool: &str) -> Self {
        Self {
            mailspace: space_slug(mailspace_name),
            pool: pool.to_string(),
        }
    }

    /// The self-test pool of this pool, in the same mailspace.
    #[must_use]
    pub fn doctor(&self) -> Self {
        Self {
            mailspace: self.mailspace.clone(),
            pool: format!("{}{DOCTOR_SUFFIX}", self.pool),
        }
    }

    /// Machine name for slot `index`: `<mailspace>-<pool>-slot-<index>`.
    #[must_use]
    pub fn slot_name(&self, index: u32) -> String {
        format!("{}-{}-slot-{index}", self.mailspace, self.pool)
    }

    /// True when `machine` carries both marks of this pool.
    #[must_use]
    pub fn owns(&self, machine: &MachineRecord) -> bool {
        let mark = |key: &str| machine.metadata.get(key).map(String::as_str);
        mark(META_POOL) == Some(self.pool.as_str())
            && mark(META_MAILSPACE) == Some(self.mailspace.as_str())
    }
}

impl std::fmt::Display for PoolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.pool)
    }
}

/// A mailspace name as it appears in marks and names: lowercase letters,
/// digits and dashes, no leading or trailing dash. Empty when the name has
/// no letter or digit (the pool config check rejects that).
#[must_use]
pub fn space_slug(name: &str) -> String {
    let mapped: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    mapped.trim_matches('-').to_string()
}

/// Metadata written onto a new slot.
#[must_use]
pub fn slot_metadata(id: &PoolId, index: u32, base: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (META_MAILSPACE.to_string(), id.mailspace.clone()),
        (META_POOL.to_string(), id.pool.clone()),
        (META_SLOT.to_string(), index.to_string()),
        (META_BASE.to_string(), base.to_string()),
        (META_STATE.to_string(), STATE_SEEDING.to_string()),
    ])
}

/// A pool member as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotStatus {
    /// The pool the slot belongs to.
    pub pool: String,
    pub name: String,
    pub id: String,
    /// `free`, `seeding`, `unseeded`, `busy`, `finished-uncollected`, or the
    /// provider's own word (see [`slot_state`]).
    pub state: String,
    pub provider_state: String,
    pub index: Option<u32>,
    pub base: Option<String>,
    /// Handle of the task holding the slot, when claimed.
    pub task: Option<String>,
    /// When the task claimed the slot (epoch seconds).
    pub claimed_at: Option<u64>,
    /// How long the open dispatch has been running, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_secs: Option<u64>,
    /// The open dispatch's time budget in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_secs: Option<u64>,
    pub created_at: String,
    pub region: String,
    /// CPU architecture of the slot, when the host reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
}

fn slot_index(machine: &MachineRecord) -> Option<u32> {
    let raw = machine.metadata.get(META_SLOT)?;
    raw.parse::<u32>().into_iter().next()
}

/// Plain-language state of a pool machine, from the provider's state word and
/// the slot's own metadata:
///
/// - `seeding`: running, and `create` has not yet marked the seed done.
/// - `unseeded`: stopped without the seed mark (seed failed or `create` was
///   interrupted; re-running `create` adopts a clean one).
/// - `busy`: claimed by a task and running (booting, working, or lingering).
/// - `finished-uncollected`: claimed by a task and stopped; the job's result
///   is still on the slot's disk.
/// - `free`: ready, stopped, unclaimed. The only claimable state.
#[must_use]
pub fn slot_state(machine: &MachineRecord) -> String {
    let seeded = machine.metadata.get(META_STATE).map(String::as_str) == Some(STATE_READY);
    let running = !matches!(machine.state.as_str(), "stopped" | "destroyed" | "failed");
    match (seeded, machine.metadata.contains_key(META_TASK), running) {
        (false, _, true) => "seeding".to_string(),
        (false, _, false) => "unseeded".to_string(),
        (true, _, true) => "busy".to_string(),
        (true, true, false) => "finished-uncollected".to_string(),
        (true, false, false) if machine.state == "stopped" => "free".to_string(),
        (true, false, false) => machine.state.clone(),
    }
}

/// True when a claim may take this machine.
#[must_use]
pub fn is_free(machine: &MachineRecord) -> bool {
    slot_state(machine) == "free"
}

/// The members of the pool `id` among `machines` (those carrying both its
/// marks), ordered by slot index then name.
#[must_use]
pub fn pool_slots(id: &PoolId, machines: &[MachineRecord]) -> Vec<SlotStatus> {
    let mut slots: Vec<SlotStatus> = machines
        .iter()
        .filter(|m| id.owns(m))
        .map(|m| SlotStatus {
            pool: id.pool.clone(),
            name: m.name.clone(),
            id: m.id.clone(),
            state: slot_state(m),
            provider_state: m.state.clone(),
            index: slot_index(m),
            base: m.metadata.get(META_BASE).cloned(),
            task: m.metadata.get(META_TASK).cloned(),
            claimed_at: m
                .metadata
                .get(META_CLAIMED)
                .and_then(|v| v.parse::<u64>().into_iter().next()),
            running_secs: None,
            budget_secs: None,
            created_at: m.created_at.clone(),
            region: m.region.clone(),
            arch: m.metadata.get(META_ARCH).cloned(),
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
