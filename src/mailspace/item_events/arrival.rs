//! Resolve arrivals to the item whose state must be settled.

use std::collections::BTreeSet;

use crate::error::VivariumError;
use crate::storage::Storage;

/// Split arrivals into event targets and ordinary work items to settle.
pub(super) fn targets(
    storage: &Storage,
    content_ids: &[String],
) -> Result<(BTreeSet<String>, BTreeSet<String>), VivariumError> {
    let mut by_event = BTreeSet::new();
    let mut by_item = BTreeSet::new();
    for content_id in content_ids {
        let Some(view) = storage.message_by_content_id(content_id)? else {
            continue;
        };
        let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
            continue;
        };
        if let Some(event) = super::parse_event(&bytes) {
            if let Some(item) = storage.live_message_by_rfc_id(&event.of)? {
                by_event.insert(item.content_id);
            }
            continue;
        }
        let named = super::parse_property_event(&bytes)
            .map(|event| event.of)
            .or_else(|| super::super::delegation::parse_assignment_event(&bytes).map(|e| e.of));
        if let Some(of) = named {
            if let Some(item) = storage.live_message_by_rfc_id(&of)? {
                by_event.insert(item.content_id);
            }
        } else if let Some(content) = super::property::verdict_report_item(storage, &bytes)? {
            by_event.insert(content);
        } else {
            by_item.insert(content_id.clone());
        }
    }
    by_item.retain(|content_id| !by_event.contains(content_id));
    Ok((by_event, by_item))
}
