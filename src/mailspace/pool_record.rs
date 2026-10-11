//! Pool dispatch records on a task.
//!
//! A dispatch and its collected result are recorded as typed events on the
//! task (`pool_dispatched`, `pool_collected`), each paired with a reply note in
//! the task's thread. The event note carries the machine-readable record; the
//! reply is the human-readable receipt. The task itself never moves.

use super::Mailspace;
use super::thread::text_body;
use crate::error::VivariumError;
use crate::storage::{MailspaceEventInput, MailspaceNoteWithReply, StoredMessageView};

/// A task as the pool reads it.
#[derive(Debug, Clone)]
pub struct PoolTask {
    pub handle: String,
    pub subject: String,
    /// The task body: the brief handed to the remote worker.
    pub body: String,
}

/// One pool event on a task, oldest first in listings.
#[derive(Debug, Clone)]
pub struct PoolEventRow {
    pub handle: String,
    pub event_type: String,
    pub note: String,
}

/// Event types the pool writes.
pub const POOL_EVENT_TYPES: [&str; 2] = ["pool_dispatched", "pool_collected"];

impl Mailspace {
    /// Read an open, unabsorbed task by handle or message id.
    ///
    /// # Errors
    /// Returns an error if the token does not resolve, the item is not a
    /// task, it is already done or absorbed, or its body cannot be read.
    pub fn pool_task(&self, token: &str) -> Result<PoolTask, VivariumError> {
        let storage = self.storage()?;
        let id = storage.resolve_message_token(token)?;
        let view = storage
            .message_by_id(&id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {token}")))?;
        if self.source_kind(&storage, &view)? != "task" {
            return Err(VivariumError::Message(format!("{token} is not a task")));
        }
        if view.local_role == "done" || view.absorbed_at.is_some() {
            return Err(VivariumError::Message(format!(
                "{token} is done or absorbed; dispatch needs an open task"
            )));
        }
        let data = storage.read_message(&id)?;
        Ok(PoolTask {
            handle: storage.display_handle(&id)?,
            subject: view.subject,
            body: text_body(&data),
        })
    }

    /// The display handle of any item, for matching against pool events.
    ///
    /// # Errors
    /// Returns an error if the token does not resolve.
    pub fn pool_handle(&self, token: &str) -> Result<String, VivariumError> {
        let storage = self.storage()?;
        let id = storage.resolve_message_token(token)?;
        storage.display_handle(&id)
    }

    /// Every pool event in the mailspace, oldest first, with display handles.
    ///
    /// # Errors
    /// Returns an error if the event log cannot be read.
    pub fn pool_events(&self) -> Result<Vec<PoolEventRow>, VivariumError> {
        let storage = self.storage()?;
        let events = storage.list_mailspace_events_of_types(&POOL_EVENT_TYPES)?;
        events
            .into_iter()
            .map(|event| {
                Ok(PoolEventRow {
                    handle: storage.display_handle(&event.message_id)?,
                    event_type: event.event_type,
                    note: event.note.unwrap_or_default(),
                })
            })
            .collect()
    }

    /// Record a pool event on a task together with a reply note carrying
    /// `text`. Both land in one transaction.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is absorbed, or the
    /// write fails.
    pub fn record_pool_note(
        &self,
        task: &str,
        event_type: &str,
        record: &str,
        text: &str,
    ) -> Result<(), VivariumError> {
        let mut storage = self.storage()?;
        let id = storage.resolve_message_token(task)?;
        let view = storage
            .message_by_id(&id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {task}")))?;
        let identity = self
            .identity_owning_account(&view.account)
            .unwrap_or_else(|| view.account.clone());
        let reply = self.note_reply(&storage, &identity, &view, text)?;
        let event = pool_event(&view, &identity, event_type, record);
        storage.note_message_with_reply(&MailspaceNoteWithReply {
            message_id: &id,
            event: &event,
            reply_requests: &reply.requests,
            reply_data: &reply.data,
            parent_content_id: &view.content_id,
        })?;
        Ok(())
    }
}

fn pool_event(
    view: &StoredMessageView,
    identity: &str,
    event_type: &str,
    record: &str,
) -> MailspaceEventInput {
    MailspaceEventInput {
        command: format!("pool {}", event_type.trim_start_matches("pool_")),
        event_type: event_type.into(),
        actor_identity: Some(identity.into()),
        account: view.account.clone(),
        message_id: view.message_id.clone(),
        content_id: view.content_id.clone(),
        from_role: Some(view.local_role.clone()),
        to_role: Some(view.local_role.clone()),
        from_identity: None,
        to_identity: Some(view.account.clone()),
        subject: view.subject.clone(),
        note: Some(record.into()),
    }
}

#[cfg(test)]
#[path = "pool_record_test.rs"]
mod tests;
