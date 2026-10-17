//! Complete-but-open needs: list them and close one on request.
//!
//! The pump knows a need's units are satisfied; Vivi owns whether the need is
//! still open. This module joins the two records without moving either one
//! until an explicit `--close`.

use serde::Serialize;

use crate::error::VivariumError;
use crate::mailspace::Mailspace;

use super::report::Snapshot;
use super::state::Status;

/// One complete need that is still open in Vivi.
#[derive(Debug, Clone, Serialize)]
pub struct CloseoutRow {
    pub handle: String,
    pub subject: String,
    pub from: String,
    pub to: String,
    pub units: usize,
    pub merged: usize,
    /// Integration tip of the newest merged unit, when one carries a commit.
    pub tip: Option<String>,
    /// Epoch seconds of the newest merge, when known.
    pub completed_at: Option<u64>,
}

/// Complete needs still open in this mailspace, sorted by handle.
///
/// # Errors
/// Returns [`VivariumError`] when the open-needs list cannot be read.
pub fn complete_open(
    mailspace: &Mailspace,
    snapshot: &Snapshot,
) -> Result<Vec<CloseoutRow>, VivariumError> {
    let complete = snapshot.complete_needs();
    let mut rows: Vec<CloseoutRow> = mailspace
        .list_kind(None, "needs", "need")?
        .into_iter()
        .filter(|message| complete.contains(&message.handle))
        .map(|message| row(snapshot, &message))
        .collect();
    rows.sort_by(|left, right| left.handle.cmp(&right.handle));
    Ok(rows)
}

/// One row from the pump's state and the open Vivi record.
fn row(snapshot: &Snapshot, message: &crate::storage::StoredMessageView) -> CloseoutRow {
    let state = snapshot.state();
    let record = state.needs.get(&message.handle);
    let units = record.map_or(0, |record| record.units.len());
    let merged = state
        .units
        .values()
        .filter(|unit| unit.need == message.handle)
        .filter(|unit| matches!(unit.status, Status::Merged | Status::NoDefect))
        .count();
    let newest = state
        .units
        .values()
        .filter(|unit| unit.need == message.handle && unit.merged_at.is_some())
        .max_by_key(|unit| unit.merged_at);
    CloseoutRow {
        handle: message.handle.clone(),
        subject: message.subject.clone(),
        from: message.from_addr.clone(),
        to: message.to_addr.clone(),
        units,
        merged,
        tip: newest.and_then(|unit| unit.commit.clone()),
        completed_at: newest.and_then(|unit| unit.merged_at),
    }
}

/// Close one complete need through the mailspace lifecycle.
///
/// # Errors
/// Returns [`VivariumError`] when the move fails (wrong identity, a foreign
/// holder, or an item that is no longer open).
pub fn close(
    mailspace: &Mailspace,
    row: &CloseoutRow,
    for_identity: Option<&str>,
) -> Result<String, VivariumError> {
    let holder = for_identity.unwrap_or_else(|| identity(&row.to));
    let tip = row.tip.as_deref().unwrap_or("unknown");
    let note = format!("pump closeout: every unit satisfied; integration tip {tip}");
    let moved =
        mailspace.move_item_outcome(holder, &row.handle, "done", Some(&note), "need done", None)?;
    let mut receipt = moved.receipt("done");
    // The holder moves the folder; the author (or an operator) accepts the
    // backlog node. A second move is a no-op once it is already settled.
    let author = identity(&row.from);
    if author != holder {
        if let Ok(accepted) = mailspace.move_item_outcome(
            author,
            &row.handle,
            "done",
            Some("pump closeout accepted"),
            "need done",
            None,
        ) {
            receipt.push('\n');
            receipt.push_str(&accepted.receipt("done"));
        }
    }
    Ok(receipt)
}

/// The local part of an address, or the whole value when it has no `@`.
fn identity(address: &str) -> &str {
    address.split('@').next().unwrap_or(address)
}
