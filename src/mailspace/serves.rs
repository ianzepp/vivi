//! The want a need serves or explores: the handle in the front matter is
//! replaced by the want's Message-ID at send, and the id is indexed in the
//! row's item metadata on every route that attaches the letter.

use std::collections::BTreeMap;

use super::Mailspace;
use super::backlog::canonical_content_handle;
use super::kind::effective_kind;
use crate::error::VivariumError;
use crate::letter::work::NeedFront;
use crate::letter::{Front, Letter};
use crate::message::normalize_message_id;
use crate::storage::Storage;

/// Resolve `serves` and `explores` to the wanted letter's Message-ID, written
/// `<id>`, and refuse a handle that is not a want.
///
/// # Errors
/// Returns an error when a handle does not resolve, names an item that is not
/// a want, or when `serves` also appears in `depends_on`.
pub(super) fn canonical_wants(
    storage: &Storage,
    front: &mut NeedFront,
) -> Result<(), VivariumError> {
    let mut serves_row = None;
    if let Some(handle) = front.serves.clone() {
        let (row, id) = want_id(storage, "serves", &handle)?;
        front.serves = Some(id);
        serves_row = Some(row);
    }
    if let Some(handle) = front.explores.clone() {
        front.explores = Some(want_id(storage, "explores", &handle)?.1);
    }
    if let Some(serves) = serves_row {
        for dep in &front.depends_on {
            let resolved = match storage.resolve_message_token(dep) {
                Ok(resolved) => resolved,
                Err(_) => continue,
            };
            if resolved == serves {
                return Err(VivariumError::Message(format!(
                    "need serves {dep} and depends on it: a need serves a want, it does not wait for it"
                )));
            }
        }
    }
    Ok(())
}

/// The row id and canonical `<id>` of the want `handle` names.
fn want_id(storage: &Storage, key: &str, handle: &str) -> Result<(String, String), VivariumError> {
    let no_such = || VivariumError::Message(format!("{key} {handle}: no such letter"));
    let Ok(resolved) = storage.resolve_message_token(handle) else {
        return Err(no_such());
    };
    let Some(view) = storage.message_by_id(&resolved)? else {
        return Err(no_such());
    };
    let data = storage.read_message(&resolved)?;
    let events = storage.list_mailspace_events(&resolved)?;
    let kind = effective_kind(&view.local_role, &data, &events).unwrap_or_else(|| "mail".into());
    if kind != "want" {
        return Err(VivariumError::Message(format!(
            "{handle} is a {kind}, not a want"
        )));
    }
    let Some(id) = view.normalized_message_id else {
        return Err(VivariumError::Message(format!(
            "{key} {handle}: the want has no Message-ID"
        )));
    };
    Ok((resolved, format!("<{id}>")))
}

impl Mailspace {
    /// The handle of this mailspace's own copy of the want `id` names, for a
    /// reader that acts on a stored `serves` or `explores`. `id` is the
    /// want's `<Message-ID>` (matched first), or a handle a legacy `for:` line
    /// held. `None` when no copy of the want is here.
    ///
    /// # Errors
    /// Returns an error when the mailspace cannot be read.
    pub fn resolve_want(&self, id: &str) -> Result<Option<String>, VivariumError> {
        let storage = self.storage()?;
        if let Some(rfc) = normalize_message_id(id)
            && let Some(view) = storage.live_message_by_rfc_id(&rfc)?
        {
            return canonical_content_handle(&storage, &view.message_id).map(Some);
        }
        match storage.resolve_message_token(id) {
            Ok(row) => Ok(Some(storage.display_handle(&row)?)),
            Err(_) => Ok(None),
        }
    }
}

/// Record a need's `serves` and `explores` in the row's item metadata, merged
/// with the keys the row already has. Any other letter is left alone.
///
/// # Errors
/// Returns an error when the row cannot be written.
pub(super) fn index_front(
    storage: &Storage,
    message_id: &str,
    letter: &Letter,
) -> Result<(), VivariumError> {
    let Front::Need(front) = &letter.front else {
        return Ok(());
    };
    let mut metadata = BTreeMap::new();
    for (key, value) in [("serves", &front.serves), ("explores", &front.explores)] {
        if let Some(id) = value.as_deref().and_then(normalize_message_id) {
            metadata.insert(key.to_string(), id);
        }
    }
    if metadata.is_empty() {
        return Ok(());
    }
    storage.set_item_metadata(message_id, &metadata)
}

#[cfg(test)]
#[path = "serves_test.rs"]
mod tests;
