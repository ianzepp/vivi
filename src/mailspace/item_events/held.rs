//! Resolve a live work item and the immutable facts its events use.

use mail_parser::MessageParser;

use super::super::delegation::DelegationState;
use super::super::holding::first_bare_address;
use super::super::kind::{effective_kind, message_kind};
use crate::error::VivariumError;
use crate::message::normalize_message_id;
use crate::storage::{Storage, StoredMessageView};

/// A work item held here: its rows and the facts events are judged by.
pub(in crate::mailspace) struct HeldItem {
    pub(in crate::mailspace) content_id: String,
    pub(in crate::mailspace) rfc_id: String,
    pub(in crate::mailspace) from_addr: String,
    /// `need`, `task`, or `want`.
    pub(in crate::mailspace) kind: &'static str,
    pub(in crate::mailspace) rows: Vec<StoredMessageView>,
    pub(in crate::mailspace) bytes: Vec<u8>,
    pub(in crate::mailspace) delegation: DelegationState,
}

/// The held work item behind `content_id`, if it is one.
pub(in crate::mailspace) fn held_item(
    storage: &Storage,
    content_id: &str,
) -> Result<Option<HeldItem>, VivariumError> {
    let rows = storage.live_messages_for_content(content_id)?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let Ok(bytes) = storage.read_listed_blob(&first.blob_relpath) else {
        return Ok(None);
    };
    let Some(kind) = work_kind(storage, &bytes, &rows)? else {
        return Ok(None);
    };
    let Some(parsed) = MessageParser::default().parse(&bytes) else {
        return Ok(None);
    };
    let Some(rfc_id) = parsed.message_id().and_then(normalize_message_id) else {
        return Ok(None);
    };
    let from_addr = first_bare_address(&first.from_addr).unwrap_or_default();
    let delegation =
        super::super::delegation::delegation_state(storage, content_id, &rfc_id, &from_addr)?;
    Ok(Some(HeldItem {
        content_id: content_id.to_string(),
        rfc_id,
        from_addr,
        kind,
        rows,
        bytes,
        delegation,
    }))
}

/// `need`, `task` or `want`: what the letter says, else its row or close event.
fn work_kind(
    storage: &Storage,
    bytes: &[u8],
    rows: &[StoredMessageView],
) -> Result<Option<&'static str>, VivariumError> {
    let named = |kind: Option<String>| match kind.as_deref() {
        Some("need") => Some("need"),
        Some("task") => Some("task"),
        Some("want") => Some("want"),
        _ => None,
    };
    if let Some(kind) = named(message_kind(bytes)) {
        return Ok(Some(kind));
    }
    for row in rows {
        let events = storage.list_mailspace_events(&row.message_id)?;
        if let Some(kind) = named(effective_kind(&row.local_role, bytes, &events)) {
            return Ok(Some(kind));
        }
    }
    Ok(None)
}
