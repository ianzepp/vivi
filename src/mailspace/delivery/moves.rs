//! Folder moves: the task/item move paths and the note reply a local
//! move writes.

use std::collections::BTreeMap;

use super::super::event_log::log_move_event;
use super::super::{Mailspace, MoveOutcome, canonical_local_role};
use super::sorted_identity_names;
use crate::error::VivariumError;
use crate::letter::plain::CloseFront;
use crate::letter::records::MailFront;
use crate::storage::{MailspaceEventInput, MailspaceMoveWithReply, Storage, StoredMessageView};

impl Mailspace {
    /// Move a task message to a new role (e.g. `done`, `tasks`). If `note` is
    /// provided, a reply message is also created.
    ///
    /// # Errors
    /// Returns an error if the identity or message cannot be resolved, the role
    /// is invalid, or the storage operation fails.
    #[allow(clippy::too_many_arguments)]
    pub fn move_task(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        note: Option<&str>,
        verdict: Option<&str>,
        repo: &[String],
        tip: &[String],
    ) -> Result<String, VivariumError> {
        self.move_task_outcome(identity, handle, role, note, verdict, repo, tip)
            .map(|moved| moved.handle)
    }

    /// [`Mailspace::move_task`], reporting whether the move travelled as an
    /// event letter or stayed a local report.
    ///
    /// # Errors
    /// Same as [`Mailspace::move_task`].
    #[allow(clippy::too_many_arguments)]
    pub fn move_task_outcome(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        note: Option<&str>,
        verdict: Option<&str>,
        repo: &[String],
        tip: &[String],
    ) -> Result<MoveOutcome, VivariumError> {
        if repo.len() != tip.len() {
            return Err(VivariumError::Other(
                "--repo and --tip must be provided in matching pairs".into(),
            ));
        }
        let mut metadata = BTreeMap::new();
        if let Some(verdict) = verdict {
            metadata.insert("verdict".into(), verdict.into());
        }
        for (repo, tip) in repo.iter().zip(tip.iter()) {
            metadata.insert(format!("repo:{repo}"), tip.clone());
        }
        self.move_item_outcome(
            identity,
            handle,
            role,
            note,
            move_command("task", role),
            if metadata.is_empty() {
                None
            } else {
                Some(&metadata)
            },
        )
    }

    fn move_event(
        command: &str,
        identity: &str,
        account: &str,
        before: &StoredMessageView,
        role: &str,
        note: &str,
    ) -> MailspaceEventInput {
        MailspaceEventInput {
            command: command.into(),
            event_type: "moved".into(),
            actor_identity: Some(identity.into()),
            account: account.into(),
            message_id: before.message_id.clone(),
            content_id: before.content_id.clone(),
            from_role: Some(before.local_role.clone()),
            to_role: Some(role.into()),
            from_identity: Some(identity.into()),
            to_identity: Some(identity.into()),
            subject: before.subject.clone(),
            note: Some(note.into()),
        }
    }

    /// Move a message to a new role with an optional note reply.
    ///
    /// # Errors
    /// Returns an error if the identity or message cannot be resolved, the role
    /// is invalid, note reply composition fails, or the storage operation fails.
    pub fn move_item(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        note: Option<&str>,
        command: &str,
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<String, VivariumError> {
        self.move_item_outcome(identity, handle, role, note, command, metadata)
            .map(|moved| moved.handle)
    }

    /// [`Mailspace::move_item`], reporting whether the move travelled as an
    /// event letter or stayed a local report, plus the handles of any bound
    /// needs the move completed through its join.
    ///
    /// # Errors
    /// Same as [`Mailspace::move_item`].
    pub fn move_item_outcome(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        note: Option<&str>,
        command: &str,
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<MoveOutcome, VivariumError> {
        let role = canonical_local_role(role)?;
        let (storage, before, identity) =
            self.move_setup(identity, handle, &role, command, metadata)?;
        let work_close = self.is_work_close(&storage, &before, &role)?;
        let mut joined_needs = Vec::new();
        // A role that may close the work (its author, or a mind) writes an
        // event letter, which carries the move to every mailspace holding
        // the item; any other move is local, and a close is a report whose
        // note carries the close facts as its `[close]` front matter.
        let close = metadata
            .map(Mailspace::close_front)
            .transpose()?
            .unwrap_or_default();
        let event = self.try_emit_for_move(
            &storage,
            &identity,
            &before,
            &role,
            note,
            command,
            close.clone(),
            &mut joined_needs,
        )?;
        if event.is_none() {
            self.refuse_sent_copy_reopen(handle, &before, &role)?;
            let note = report_note(note, &close);
            self.move_row_locally(&storage, &identity, &before, &role, note, command, &close)?;
        }
        Self::store_move_metadata(&storage, &before.message_id, metadata)?;
        let shown = storage.display_handle(&before.message_id)?;
        if event.is_none() {
            self.sync_backlog_after_move(
                &storage,
                &identity,
                &before,
                &role,
                &shown,
                &mut joined_needs,
            )?;
        }
        Ok(MoveOutcome {
            handle: shown,
            reported: event.is_none() && work_close,
            event,
            joined_needs,
        })
    }

    /// A move's own checks and its target row: the metadata shape, the
    /// identity, the storage, and the row `identity` holds under `handle`
    /// (refusing an absorbed one).
    fn move_setup(
        &self,
        identity: &str,
        handle: &str,
        role: &str,
        command: &str,
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<(std::cell::Ref<'_, Storage>, StoredMessageView, String), VivariumError> {
        Self::validate_move_metadata(metadata)?;
        let identity = self.resolve_identity(identity)?;
        let storage = self.storage()?;
        let before = self.move_target(&storage, &identity, handle, role, command)?;
        if before.absorbed_at.is_some() {
            return Err(VivariumError::Message(format!(
                "{handle} is absorbed and can no longer be changed"
            )));
        }
        Ok((storage, before, identity))
    }

    /// Check that close provenance arrives as one nonempty branch/commit pair.
    fn validate_move_metadata(
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<(), VivariumError> {
        if metadata
            .is_some_and(|fields| fields.contains_key("branch") != fields.contains_key("commit"))
        {
            return Err(VivariumError::Other(
                "branch and commit close metadata must be provided together".into(),
            ));
        }
        if metadata.is_some_and(|fields| {
            [fields.get("branch"), fields.get("commit")]
                .into_iter()
                .flatten()
                .any(|value| value.trim().is_empty())
        }) {
            return Err(VivariumError::Other(
                "branch and commit close metadata cannot be empty".into(),
            ));
        }
        Ok(())
    }

    /// Persist close provenance after its move succeeds, when supplied.
    fn store_move_metadata(
        storage: &Storage,
        message_id: &str,
        metadata: Option<&BTreeMap<String, String>>,
    ) -> Result<(), VivariumError> {
        if let Some(metadata) = metadata {
            storage.set_item_metadata(message_id, metadata)?;
        }
        Ok(())
    }

    /// The row a move acts on: the one the identity holds under `handle`, or
    /// any row of the same item when the identity may close it.
    fn move_target(
        &self,
        storage: &Storage,
        identity: &str,
        handle: &str,
        role: &str,
        command: &str,
    ) -> Result<StoredMessageView, VivariumError> {
        let names = sorted_identity_names(self.identity_names(identity));
        let Ok(resolved) = storage.resolve_message_token_for_accounts(handle, &names) else {
            return self
                .row_for_item_actor(storage, identity, handle, role)?
                .ok_or_else(|| self.foreign_move_error(storage, identity, handle, role, command));
        };
        let Some(before) = storage.message_by_id(&resolved)? else {
            return Err(VivariumError::Message(format!(
                "message not found: {handle}"
            )));
        };
        if !names.contains(&before.account) {
            return Err(VivariumError::Message(format!(
                "message not found for {identity}: {handle}"
            )));
        }
        Ok(before)
    }

    /// Move the one row `before` names, with the note as a reply when given.
    /// A note with close metadata is the report letter: its `[close]` front
    /// carries the facts to every mailspace holding the item.
    fn move_row_locally(
        &self,
        storage: &Storage,
        identity: &str,
        before: &StoredMessageView,
        role: &str,
        note: Option<&str>,
        command: &str,
        close: &CloseFront,
    ) -> Result<(), VivariumError> {
        // Stored messages keep the account name they were ingested under
        // even after a rename, so the storage-layer mutation must target
        // that historical account rather than the current canonical name.
        let account = before.account.clone();
        let Some(note) = note else {
            storage.move_message_to_role(&account, &before.message_id, role)?;
            return log_move_event(storage, identity, role, before, command, None);
        };
        let reply = self.note_reply(storage, identity, before, note, report_front(close))?;
        let event = Self::move_event(command, identity, &account, before, role, note);
        storage.move_message_with_reply(&MailspaceMoveWithReply {
            account: &account,
            message_id: &before.message_id,
            local_role: role,
            event: &event,
            reply_requests: &reply.requests,
            reply_data: &reply.data,
            parent_content_id: &before.content_id,
        })?;
        Ok(())
    }

    /// Error for a token that resolves globally but is not held by the
    /// acting identity: name the holder so the operator can retry with
    /// `--for <holder>`. Falls back to a plain not-found error.
    pub(in crate::mailspace) fn cross_identity_error(
        &self,
        storage: &Storage,
        token: &str,
    ) -> VivariumError {
        let Ok(message_id) = storage.resolve_message_token(token) else {
            return VivariumError::Message(format!("message not found: {token}"));
        };
        let holder = match storage.message_by_id(&message_id) {
            Ok(Some(view)) => self.identity_owning_account(&view.account),
            _ => None,
        };
        match holder {
            Some(holder) => VivariumError::Message(format!(
                "message '{token}' is held by identity '{holder}'; retry with --for {holder}"
            )),
            None => VivariumError::Message(format!("message not found: {token}")),
        }
    }
}

/// A verdict-only report still sends a letter, so the owner sees the verdict.
fn report_note<'a>(note: Option<&'a str>, close: &CloseFront) -> Option<&'a str> {
    note.or(has_close(close).then_some("done"))
}

/// The front matter of a note reply: the close facts of a local report as
/// the note's `[close]` table, none for a plain move note.
fn report_front(close: &CloseFront) -> MailFront {
    MailFront {
        close: has_close(close).then(|| close.clone()),
        ..MailFront::default()
    }
}

/// Whether the close metadata carries any fact.
fn has_close(close: &CloseFront) -> bool {
    close != &CloseFront::default()
}

fn move_command(kind: &str, role: &str) -> &'static str {
    match (kind, role) {
        ("task", "done") => "task done",
        ("task", _) => "task reopen",
        _ => "item move",
    }
}
