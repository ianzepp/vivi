//! Item facts shared by the pump record and the flow mail port: the rows of
//! one item with their events, whether the item is done, and its folded
//! marks. This is the one copy of the done rule (the newest folder move
//! wins, else any row in `done`) and of the event-ordered marks fold.

use crate::error::VivariumError;
use crate::storage::{MailspaceEvent, Storage, StoredMessageView};

/// Every live row of one item and every event logged against those rows.
pub(crate) struct HeldItem {
    pub(crate) rows: Vec<StoredMessageView>,
    pub(crate) events: Vec<MailspaceEvent>,
}

/// The item behind `handle`, or `None` when no message answers to it.
///
/// # Errors
/// Returns an error when the handle cannot be resolved or the rows and events
/// cannot be read.
pub(crate) fn held_item_if_present(
    storage: &Storage,
    handle: &str,
) -> Result<Option<HeldItem>, VivariumError> {
    let message_id = storage.resolve_message_token(handle)?;
    let Some(view) = storage.message_by_id(&message_id)? else {
        return Ok(None);
    };
    let rows = storage.live_messages_for_content(&view.content_id)?;
    let mut events = Vec::new();
    for row in &rows {
        events.extend(storage.list_mailspace_events(&row.message_id)?);
    }
    Ok(Some(HeldItem { rows, events }))
}

/// The item behind `handle`.
///
/// # Errors
/// Returns `message not found: <handle>` when no message answers to it, or an
/// error when the rows and events cannot be read.
pub(crate) fn held_item(storage: &Storage, handle: &str) -> Result<HeldItem, VivariumError> {
    held_item_if_present(storage, handle)?
        .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))
}

/// The newest folder move of the item, close or reopen alike.
pub(crate) fn newest_move(events: &[MailspaceEvent]) -> Option<&MailspaceEvent> {
    events
        .iter()
        .filter(|event| event.event_type == "moved" && event.to_role.is_some())
        .max_by(|a, b| (&a.occurred_at, a.event_id).cmp(&(&b.occurred_at, b.event_id)))
}

/// Whether the item is done: the newest folder move decides, else any row
/// that sits in `done`.
pub(crate) fn is_done(item: &HeldItem) -> bool {
    match newest_move(&item.events) {
        Some(event) => event.to_role.as_deref() == Some("done"),
        None => item.rows.iter().any(|row| row.local_role == "done"),
    }
}

/// The item's marks: its events in order, folded.
pub(crate) fn sorted_marks(item: &HeldItem) -> Vec<String> {
    let mut events = item.events.clone();
    events.sort_by(|a, b| (&a.occurred_at, a.event_id).cmp(&(&b.occurred_at, b.event_id)));
    super::marks::folded_marks(&events)
}

#[cfg(test)]
#[path = "item_facts_test.rs"]
mod tests;
