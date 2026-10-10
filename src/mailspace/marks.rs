//! Append-only marks on a need or a want.
//!
//! A mark is an event. The message bytes stay as sent. The folded set is the
//! latest `marked` or `unmarked` event per slug, in log order.

use std::collections::BTreeSet;

use super::Mailspace;
use crate::error::VivariumError;
use crate::storage::{MailspaceEvent, MailspaceEventInput, StoredMessageView};

/// One slug after a `mark` or `unmark` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkWrite {
    /// Display handle of the item.
    pub handle: String,
    /// Slug the command named.
    pub slug: String,
    /// Whether this command appended an event for the slug.
    pub changed: bool,
}

/// Reject a slug outside the role-label character set.
///
/// Comparison stays case-sensitive. `bug` and `Bug` are different marks.
///
/// # Errors
/// Returns an error when the slug is empty or contains any other character.
pub fn validate_mark(value: &str) -> Result<(), VivariumError> {
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(VivariumError::Message(format!(
            "invalid mark '{value}'; use letters, numbers, dot, dash, or underscore"
        )))
    }
}

/// Validate a list filter. An empty filter matches every item.
///
/// # Errors
/// Returns an error when any slug fails [`validate_mark`].
pub fn validate_marks_filter(slugs: &[String]) -> Result<(), VivariumError> {
    for slug in slugs {
        validate_mark(slug)?;
    }
    Ok(())
}

/// Validate every slug. An empty list fails before any write.
///
/// # Errors
/// Returns an error when `slugs` is empty or any slug fails [`validate_mark`].
pub fn validate_marks(slugs: &[String]) -> Result<(), VivariumError> {
    if slugs.is_empty() {
        return Err(VivariumError::Message(
            "at least one mark is required".into(),
        ));
    }
    for slug in slugs {
        validate_mark(slug)?;
    }
    Ok(())
}

/// Fold mark events into the current slug set, sorted.
///
/// Walk `events` in log order. `marked` adds the note as a slug. `unmarked`
/// removes it. Every other event is ignored. A duplicated `marked` row still
/// folds to present.
#[must_use]
pub fn folded_marks(events: &[MailspaceEvent]) -> Vec<String> {
    let mut present = BTreeSet::new();
    for event in events {
        let Some(slug) = event.note.as_deref().filter(|slug| !slug.is_empty()) else {
            continue;
        };
        match event.event_type.as_str() {
            "marked" => {
                present.insert(slug.to_string());
            }
            "unmarked" => {
                present.remove(slug);
            }
            _ => {}
        }
    }
    present.into_iter().collect()
}

/// True when `marks` contains every slug in `required`.
#[must_use]
pub fn marks_include(marks: &[String], required: &[String]) -> bool {
    required
        .iter()
        .all(|slug| marks.iter().any(|mark| mark == slug))
}

impl Mailspace {
    /// Append mark or unmark events for `kind` (`need` or `want`).
    ///
    /// Slugs already in the requested state append nothing. Every slug that
    /// does change is written in one transaction.
    ///
    /// # Errors
    /// Returns an error when a slug is invalid, the identity does not hold
    /// the item, the item is the wrong kind, the item is absorbed, or a
    /// storage operation fails. A failure writes no events.
    pub fn set_marks(
        &self,
        identity: &str,
        handle: &str,
        kind: &str,
        add: bool,
        slugs: &[String],
    ) -> Result<Vec<MarkWrite>, VivariumError> {
        validate_marks(slugs)?;
        let (identity, message) = self.resolve_owned_message(identity, handle)?;
        let mut storage = self.storage()?;
        let actual = self.source_kind(&storage, &message)?;
        if actual != kind {
            return Err(VivariumError::Message(format!("{handle} is not a {kind}")));
        }
        if message.absorbed_at.is_some() {
            return Err(VivariumError::Message(format!(
                "{handle} is absorbed and can no longer be changed"
            )));
        }
        let events = storage.list_mailspace_events(&message.message_id)?;
        let planned = plan_mark_changes(&events, slugs, add);
        let inputs = planned
            .iter()
            .filter(|change| change.changed)
            .map(|change| mark_event(&message, &identity, kind, add, &change.slug))
            .collect::<Vec<_>>();
        storage.append_mailspace_events(&inputs)?;
        let display = storage.display_handle(&message.message_id)?;
        Ok(planned
            .into_iter()
            .map(|change| MarkWrite {
                handle: display.clone(),
                slug: change.slug,
                changed: change.changed,
            })
            .collect())
    }
}

struct MarkChange {
    slug: String,
    changed: bool,
}

fn plan_mark_changes(events: &[MailspaceEvent], slugs: &[String], add: bool) -> Vec<MarkChange> {
    let mut present: BTreeSet<String> = folded_marks(events).into_iter().collect();
    let mut changes = Vec::with_capacity(slugs.len());
    for slug in slugs {
        let changed = if add {
            present.insert(slug.clone())
        } else {
            present.remove(slug)
        };
        changes.push(MarkChange {
            slug: slug.clone(),
            changed,
        });
    }
    changes
}

fn mark_event(
    message: &StoredMessageView,
    identity: &str,
    kind: &str,
    add: bool,
    slug: &str,
) -> MailspaceEventInput {
    let verb = if add { "mark" } else { "unmark" };
    MailspaceEventInput {
        command: format!("{kind} {verb}"),
        event_type: if add { "marked" } else { "unmarked" }.into(),
        actor_identity: Some(identity.to_string()),
        account: message.account.clone(),
        message_id: message.message_id.clone(),
        content_id: message.content_id.clone(),
        from_role: Some(message.local_role.clone()),
        to_role: Some(message.local_role.clone()),
        from_identity: None,
        to_identity: Some(message.account.clone()),
        subject: message.subject.clone(),
        note: Some(slug.to_string()),
    }
}

#[cfg(test)]
#[path = "marks_test.rs"]
mod tests;
