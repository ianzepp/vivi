//! The join guard: a need whose `bound` letter names a unit this mailspace has
//! never seen is not join-complete, however done the other units are. The unit's
//! task letter may still arrive; completing the need before then would close it
//! on an incomplete view of its units.

use super::parse_property_event;
use super::property::PropertyKind;
use crate::error::VivariumError;
use crate::storage::Storage;

/// True when a `bound` letter held as a reply to the need `need_source` names a
/// unit that has never been seen on this member: no message row at all. A unit
/// that is held but absorbed, or no longer live as work, counts as settled and
/// does not block the join. A need that cannot be resolved here never blocks.
///
/// # Errors
/// Returns a [`VivariumError`] on storage failure.
pub(in crate::mailspace) fn named_units_unseen(
    storage: &Storage,
    need_source: &str,
) -> Result<bool, VivariumError> {
    let Ok(id) = storage.resolve_message_token(need_source) else {
        return Ok(false);
    };
    let Some(need) = storage.message_by_id(&id)? else {
        return Ok(false);
    };
    let Some(need_rfc_id) = need.normalized_message_id.as_deref() else {
        return Ok(false);
    };
    for child in storage.reply_child_contents(&need.content_id)? {
        let Some(view) = storage.message_by_content_id(&child)? else {
            continue;
        };
        let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
            continue;
        };
        let Some(event) = parse_property_event(&bytes) else {
            continue;
        };
        if event.kind != PropertyKind::Bound || event.of != need_rfc_id {
            continue;
        }
        for unit in &event.units {
            if storage.live_message_by_rfc_id(unit)?.is_none() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
#[path = "join_guard_test.rs"]
mod tests;
