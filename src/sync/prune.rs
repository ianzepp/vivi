//! `vivi sync prune`: evict old letters bound to the project from the local
//! store, restorable byte for byte.
//!
//! The policy is conservative and always explicit (nothing prunes on its
//! own): never open needs, tasks or wants (nor the sender's own copy of open
//! work handed to another mailspace), never starred or absorbed items, never
//! anything without a binding (local-origin letters), only letters dated
//! before the cutoff. The row and its local state stay and the blob goes;
//! `resync`, `fetch`, or `pull --refs` bring a letter back as a restored row.
//! It needs neither the server nor a key.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::StoredMessageView;
use crate::sync::PROVIDER;

/// Listing lines shown before the rest is summarized.
const LISTING: usize = 20;

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

/// One letter selected for eviction.
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

/// Evict letters bound to the project older than `age` as of `now`. The
/// project is the configured `[sync] project` when set, else any project. A
/// dry run reports the same selection and changes nothing.
///
/// # Errors
/// Returns an error for an unusable age or a storage failure.
pub fn prune(
    mailspace: &Mailspace,
    age: Duration,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<PruneReport, VivariumError> {
    let project = mailspace
        .config
        .sync
        .as_ref()
        .and_then(|sync| sync.project.as_deref())
        .map(str::trim)
        .filter(|project| !project.is_empty());
    let cutoff = cutoff_string(now, age)?;
    let storage = mailspace.storage()?;
    // Open work in flight is kept whichever folder holds it: the sender's
    // `sent` copy of work handed to another mailspace is the item here. A
    // `done` row is closed work by its folder, whatever its node says.
    let in_flight = mailspace.open_work_content_ids()?;
    let mut candidates = storage.eviction_candidates(PROVIDER, project, &KEEP_ROLES, &cutoff)?;
    candidates.retain(|row| row.local_role == "done" || !in_flight.contains(&row.content_id));
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

/// The operator lines for a report.
#[must_use]
pub fn report_lines(report: &PruneReport) -> Vec<String> {
    let verb = if report.dry_run {
        "would evict"
    } else {
        "evicted"
    };
    let count = if report.dry_run {
        report.items.len()
    } else {
        report.evicted
    };
    let mut lines = vec![format!(
        "{verb} {count} letter(s) dated before {} (open needs/tasks/wants, including your copies \
         of work sent to another mailspace, starred, absorbed, and local mail are kept)",
        report.cutoff
    )];
    for item in report.items.iter().take(LISTING) {
        lines.push(format!(
            "  {} {} {} {}",
            item.handle, item.role, item.date, item.subject
        ));
    }
    if report.items.len() > LISTING {
        lines.push(format!("  ... and {} more", report.items.len() - LISTING));
    }
    if report.dry_run {
        lines.push("dry run: nothing changed".into());
    } else if report.evicted > 0 {
        lines.push(
            "vivi sync resync, vivi sync fetch <handle>, or vivi sync pull --refs restores \
             evicted letters from the server"
                .into(),
        );
    }
    lines
}

#[cfg(test)]
#[path = "prune_test.rs"]
mod tests;
