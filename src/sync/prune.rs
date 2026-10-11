//! `vivi sync prune`: evict old letters bound to the project from the local
//! store, restorable byte for byte.
//!
//! The policy and the eviction are chat prune's: never open needs, tasks or
//! wants, never starred or absorbed items, never anything without a binding
//! (local-origin letters), only letters dated before the cutoff. The row and
//! its local state stay and the blob goes; `resync`, `fetch`, or
//! `pull --refs` bring a letter back as a restored row. It needs neither the
//! server nor a key and never runs on its own.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::chat::prune::{PruneReport, evict_bound_before};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::sync::PROVIDER;

/// Listing lines shown before the rest is summarized.
const LISTING: usize = 20;

/// Evict letters bound to the project older than `age` as of `now`. The
/// project is the configured `[sync] project` when set, else any project.
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
    evict_bound_before(mailspace, PROVIDER, project, age, now, dry_run)
}

/// The operator lines for a report, like `vivi chat prune`.
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
        "{verb} {count} letter(s) dated before {} (open needs/tasks/wants, starred, absorbed, \
         and local mail are kept)",
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
