//! Local cache eviction for room items.
//!
//! A room is the durable log; the mailspace holds a replica of it (D5).
//! Pruning evicts old replicas to reclaim space and is always safe because
//! `vivi chat resync` rebuilds them byte-identically from the room.
//!
//! The policy is deliberately conservative and always explicit (nothing
//! prunes on its own):
//! - only items bound to a `vivi-room` mailbox; local-origin mail is never
//!   touched;
//! - never open work: anything still in `needs`, `tasks`, or `wants` is live
//!   state, however old;
//! - never starred or absorbed items;
//! - only items dated before the cutoff.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::chat::client::normalize_room_id;
use crate::chat::item::PROVIDER;
use crate::duration::parse_duration;
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::StoredMessageView;

/// Folders that hold live work and are never evicted.
const KEEP_ROLES: [&str; 3] = ["needs", "tasks", "wants"];

/// What a prune evicted, or would evict on a dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub cutoff: String,
    pub dry_run: bool,
    pub items: Vec<PrunedItem>,
    pub evicted: usize,
}

/// One item selected for eviction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedItem {
    pub handle: String,
    pub role: String,
    pub date: String,
    pub subject: String,
}

impl From<&StoredMessageView> for PrunedItem {
    fn from(view: &StoredMessageView) -> Self {
        Self {
            handle: view.handle.clone(),
            role: view.local_role.clone(),
            date: view.date.clone(),
            subject: view.subject.clone(),
        }
    }
}

/// Parse an age such as `30d`, `12h`, `90m`. Days are accepted here because a
/// cache age is naturally counted in days; every other form goes through the
/// shared interval parser.
///
/// # Errors
/// Returns a config error for a zero, malformed, or unknown-unit age.
pub fn parse_age(value: &str) -> Result<Duration, VivariumError> {
    let trimmed = value.trim();
    if let Some(days) = trimmed.strip_suffix('d') {
        return match days.parse::<u64>() {
            Ok(days) if days > 0 => Ok(Duration::from_secs(days.saturating_mul(24 * 60 * 60))),
            _ => Err(VivariumError::Config(format!(
                "invalid age '{value}'; use values like 30d, 12h, or 90m"
            ))),
        };
    }
    parse_duration(trimmed)
}

/// Evict room items older than `age` as of `now`. `room` narrows to one room
/// (it need not still be joined: items outlive `chat leave`). A dry run
/// reports the same selection and changes nothing.
///
/// # Errors
/// Returns an error for an unusable room id or age, or a storage failure.
pub fn prune(
    mailspace: &Mailspace,
    room: Option<&str>,
    age: Duration,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<PruneReport, VivariumError> {
    let room_id = room.map(normalize_room_id).transpose()?;
    evict_bound_before(mailspace, PROVIDER, room_id.as_deref(), age, now, dry_run)
}

/// The shared selection and eviction: rows bound to `provider` (narrowed to
/// one remote `mailbox` when given) dated before `now - age`, under the
/// conservative policy above. A dry run reports the same selection and
/// changes nothing. Used by `vivi chat prune` and `vivi sync prune`.
///
/// # Errors
/// Returns an error for an unusable age or a storage failure.
pub fn evict_bound_before(
    mailspace: &Mailspace,
    provider: &str,
    mailbox: Option<&str>,
    age: Duration,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<PruneReport, VivariumError> {
    let cutoff = cutoff_string(now, age)?;
    let mut storage = mailspace.storage()?;
    let candidates = storage.eviction_candidates(provider, mailbox, &KEEP_ROLES, &cutoff)?;
    let items = candidates.iter().map(PrunedItem::from).collect();
    let evicted = if dry_run {
        0
    } else {
        let ids: Vec<String> = candidates.into_iter().map(|c| c.message_id).collect();
        storage.evict_messages(&ids)?
    };
    Ok(PruneReport {
        cutoff,
        dry_run,
        items,
        evicted,
    })
}

/// The cutoff in the exact form message metadata stores dates (`to_rfc3339`
/// of a whole-second UTC instant), so the comparison in SQL is lexical.
fn cutoff_string(now: DateTime<Utc>, age: Duration) -> Result<String, VivariumError> {
    let span = chrono::Duration::from_std(age)
        .map_err(|_| VivariumError::Config("age is too large".into()))?;
    let cutoff = now
        .checked_sub_signed(span)
        .and_then(|at| DateTime::from_timestamp(at.timestamp(), 0))
        .ok_or_else(|| VivariumError::Config("age is too large".into()))?;
    Ok(cutoff.to_rfc3339())
}

#[cfg(test)]
#[path = "prune_test.rs"]
mod tests;
