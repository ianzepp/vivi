//! What the host says about its slots, as the pool's [`MachineRecord`].
//!
//! The remote script prints one block per slot: a `@@slot <id>` line, then
//! `key=value` lines (`state`, `exit`, `created`, `mark.<name>` for each mark
//! file, `claim.task` and `claim.claimed` for the claim). Marks keep the
//! names the pool already uses for machine metadata, so they pass straight
//! through.

use std::collections::BTreeMap;

use super::provider::MachineRecord;
use super::slot::{META_CLAIMED, META_TASK};
use crate::error::VivariumError;

const SLOT_MARKER: &str = "@@slot ";
/// The directory name of slot `index` inside its pool directory.
const SLOT_DIR_PREFIX: &str = "slot-";

/// The id of a slot: its path below the root, `<pool>/slot-<index>`.
#[must_use]
pub fn slot_id(pool: &str, index: &str) -> String {
    format!("{pool}/{SLOT_DIR_PREFIX}{index}")
}

/// What the pool calls slot `id`: `<pool>-slot-<index>`.
#[must_use]
pub fn name_of(id: &str) -> String {
    id.replacen(
        &format!("/{SLOT_DIR_PREFIX}"),
        &format!("-{SLOT_DIR_PREFIX}"),
        1,
    )
}

/// True for `<pool>/slot-<digits>` with a pool of lowercase letters, digits
/// and dashes: the only ids the remote script accepts.
#[must_use]
pub fn is_slot_id(id: &str) -> bool {
    let Some((pool, slot)) = id.split_once('/') else {
        return false;
    };
    let index = slot.strip_prefix(SLOT_DIR_PREFIX);
    !pool.starts_with('-')
        && !pool.is_empty()
        && pool
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && index.is_some_and(|i| !i.is_empty() && i.chars().all(|c| c.is_ascii_digit()))
}

/// Parse the listing of the remote script. `host` is what the listing's
/// `region` column shows.
///
/// # Errors
/// Returns an error if a `key=value` line comes before any `@@slot` line.
pub fn parse_listing(text: &str, host: &str) -> Result<Vec<MachineRecord>, VivariumError> {
    let mut records: Vec<MachineRecord> = Vec::new();
    for line in text.lines() {
        if let Some(id) = line.strip_prefix(SLOT_MARKER) {
            records.push(blank(id.trim(), host));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let record = records.last_mut().ok_or_else(|| {
            VivariumError::Parse(format!("unexpected line in the slot listing: {line}"))
        })?;
        apply(record, key, value.trim());
    }
    Ok(records)
}

fn blank(id: &str, host: &str) -> MachineRecord {
    MachineRecord {
        id: id.to_string(),
        name: name_of(id),
        state: "stopped".to_string(),
        region: host.to_string(),
        created_at: String::new(),
        metadata: BTreeMap::new(),
        exit_code: None,
    }
}

fn apply(record: &mut MachineRecord, key: &str, value: &str) {
    match key {
        "state" => record.state = value.to_string(),
        "exit" => record.exit_code = value.parse::<i64>().into_iter().next(),
        "created" => record.created_at = value.to_string(),
        "claim.task" => {
            record
                .metadata
                .insert(META_TASK.to_string(), value.to_string());
        }
        "claim.claimed" => {
            record
                .metadata
                .insert(META_CLAIMED.to_string(), value.to_string());
        }
        _ => {
            if let Some(mark) = key.strip_prefix("mark.")
                && !value.is_empty()
            {
                record.metadata.insert(mark.to_string(), value.to_string());
            }
        }
    }
}

#[cfg(test)]
#[path = "ssh_state_test.rs"]
mod tests;
