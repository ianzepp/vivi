//! Idempotent ingest and local eviction for synced (room-sourced) items.
//!
//! A synced item is a replica of an append-only log entry (D5): the same id
//! always names the same bytes, so re-ingesting one is a no-op and must never
//! reset local state (`local_role`, `read_state`, `starred`). Eviction is a
//! local cache operation: the row is soft-deleted and its blob dropped, and a
//! later re-ingest restores both without touching that local state.

use rusqlite::TransactionBehavior;

use super::events::append_event_tx;
use super::ingest::{
    ingest_message_id, upsert_blob_row, upsert_message_row, upsert_metadata_row,
    upsert_remote_binding,
};
use super::{
    MailspaceEventInput, MessageIngestRequest, OptionalExtension, Storage, StoredMessage,
    StoredMessageView, Utc, VivariumError, blob_relpath, params, parse_metadata, sha256_hex,
    write_blob_if_absent,
};

/// What [`Storage::ingest_if_absent`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// No row held this id; a new message row was created.
    Inserted,
    /// An evicted row held this id; its blob and visibility were restored and
    /// its local state kept.
    Restored,
    /// A live row already held this id; nothing changed.
    Existing,
}

/// The delivery event to append atomically with an insert or restore. The
/// recipient fields come from the request.
#[derive(Debug, Clone, Copy)]
pub struct IngestEvent<'a> {
    pub command: &'a str,
    pub event_type: &'a str,
    pub subject: &'a str,
    pub note: Option<&'a str>,
}

impl Storage {
    /// Ingest one synced item unless a live row already holds its id.
    ///
    /// Unlike [`Storage::ingest_message`], an existing row is left exactly as
    /// it is. An evicted row (soft-deleted by [`Storage::evict_messages`]) is
    /// restored in place. The optional `event` is appended in the same
    /// transaction, and only when a row was inserted or restored.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if a presented id is malformed, the id
    /// already names different content, or the database or blob write fails.
    pub fn ingest_if_absent(
        &mut self,
        request: &MessageIngestRequest,
        data: &[u8],
        event: Option<&IngestEvent<'_>>,
    ) -> Result<(IngestOutcome, StoredMessage), VivariumError> {
        let content_id = sha256_hex(data);
        let message_id = ingest_message_id(request, &content_id)?;
        let blob_relpath = blob_relpath(&content_id);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| VivariumError::Other(format!("failed to open sync transaction: {e}")))?;
        let outcome = match held_row(&tx, &message_id)? {
            None => IngestOutcome::Inserted,
            Some((held, _)) if held != content_id => {
                return Err(VivariumError::Message(format!(
                    "id {message_id} already names different content in this mailspace"
                )));
            }
            Some((_, None)) => IngestOutcome::Existing,
            Some((_, Some(_))) => IngestOutcome::Restored,
        };
        let mut created_blob = false;
        if outcome != IngestOutcome::Existing {
            created_blob = write_blob_if_absent(&self.mail_root.join(&blob_relpath), data)?;
            let metadata = parse_metadata(data);
            let now = Utc::now().to_rfc3339();
            upsert_blob_row(&tx, &content_id, &blob_relpath, data.len(), &metadata, &now)?;
            upsert_metadata_row(&tx, &content_id, &metadata)?;
            if outcome == IngestOutcome::Inserted {
                upsert_message_row(&tx, request, &message_id, &content_id, &now)?;
            } else {
                restore_message_row(&tx, &message_id, &now)?;
            }
            upsert_remote_binding(&tx, &message_id, request.remote.as_ref(), &now)?;
            if let Some(event) = event {
                let input = delivery_event(event, request, &message_id, &content_id);
                append_event_tx(&tx, &input, &now)?;
            }
        }
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit sync transaction: {e}")))?;
        if outcome != IngestOutcome::Existing {
            self.invalidate_handle_index();
        }
        let stored = StoredMessage {
            message_id,
            content_id,
            blob_relpath,
            created_blob,
        };
        Ok((outcome, stored))
    }

    /// Live messages bound to `provider` that are safe to evict: not
    /// absorbed, not starred, not in one of `keep_roles`, and dated before
    /// `before` (an RFC 3339 UTC string as stored in message metadata).
    /// `mailbox` narrows to one remote mailbox (a room).
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn eviction_candidates(
        &self,
        provider: &str,
        mailbox: Option<&str>,
        keep_roles: &[&str],
        before: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let roles = keep_roles
            .iter()
            .map(|role| format!("'{}'", role.replace('\'', "")))
            .collect::<Vec<_>>()
            .join(", ");
        let clause = format!(
            "WHERE rb.provider = ?1 AND (?2 IS NULL OR rb.remote_mailbox = ?2)
             AND m.deleted_at IS NULL AND m.absorbed_at IS NULL AND m.starred = 0
             AND m.local_role NOT IN ({roles}) AND md.date != '' AND md.date < ?3"
        );
        self.list_messages_by_query(&clause, params![provider, mailbox, before])
    }

    /// Evict messages from the local cache: soft-delete each live,
    /// non-absorbed row and drop any blob no live row still references. The
    /// `blobs` and `messages` rows stay, so [`Storage::ingest_if_absent`] can
    /// restore the item byte-identically. Returns how many rows were evicted.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database update fails.
    pub fn evict_messages(&mut self, message_ids: &[String]) -> Result<usize, VivariumError> {
        let now = Utc::now().to_rfc3339();
        let tx = self
            .conn
            .transaction()
            .map_err(|e| VivariumError::Other(format!("failed to open evict transaction: {e}")))?;
        let mut evicted = 0;
        let mut content_ids = Vec::new();
        for message_id in message_ids {
            let content_id: Option<String> = tx
                .query_row(
                    "SELECT content_id FROM messages
                     WHERE message_id = ?1 AND deleted_at IS NULL AND absorbed_at IS NULL",
                    params![message_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| VivariumError::Other(format!("failed to read eviction row: {e}")))?;
            let Some(content_id) = content_id else {
                continue;
            };
            tx.execute(
                "UPDATE messages SET deleted_at = ?2, updated_at = ?2 WHERE message_id = ?1",
                params![message_id, now],
            )
            .map_err(|e| VivariumError::Other(format!("failed to evict message: {e}")))?;
            content_ids.push(content_id);
            evicted += 1;
        }
        let unreferenced = unreferenced_blobs(&tx, content_ids)?;
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit eviction: {e}")))?;
        for content_id in unreferenced {
            self.drop_blob_file(&content_id)?;
        }
        self.invalidate_handle_index();
        Ok(evicted)
    }

    /// Remove a blob file from disk. A missing file is already evicted.
    fn drop_blob_file(&self, content_id: &str) -> Result<(), VivariumError> {
        match std::fs::remove_file(self.mail_root.join(blob_relpath(content_id))) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(VivariumError::Io(e)),
            _ => Ok(()),
        }
    }
}

/// The stored content id and `deleted_at` for an id, if any row holds it.
fn held_row(
    tx: &rusqlite::Transaction<'_>,
    message_id: &str,
) -> Result<Option<(String, Option<String>)>, VivariumError> {
    tx.query_row(
        "SELECT content_id, deleted_at FROM messages WHERE message_id = ?1",
        params![message_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|e| VivariumError::Other(format!("failed to check held id: {e}")))
}

/// Make an evicted row visible again, leaving its local state untouched.
fn restore_message_row(
    tx: &rusqlite::Transaction<'_>,
    message_id: &str,
    now: &str,
) -> Result<(), VivariumError> {
    tx.execute(
        "UPDATE messages SET deleted_at = NULL, updated_at = ?2 WHERE message_id = ?1",
        params![message_id, now],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to restore message row: {e}")))
}

/// The distinct content ids in `content_ids` that no live row references.
fn unreferenced_blobs(
    tx: &rusqlite::Transaction<'_>,
    mut content_ids: Vec<String>,
) -> Result<Vec<String>, VivariumError> {
    content_ids.sort();
    content_ids.dedup();
    let mut unreferenced = Vec::new();
    for content_id in content_ids {
        let live: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE content_id = ?1 AND deleted_at IS NULL",
                params![content_id],
                |row| row.get(0),
            )
            .map_err(|e| VivariumError::Other(format!("failed to count blob refs: {e}")))?;
        if live == 0 {
            unreferenced.push(content_id);
        }
    }
    Ok(unreferenced)
}

fn delivery_event(
    event: &IngestEvent<'_>,
    request: &MessageIngestRequest,
    message_id: &str,
    content_id: &str,
) -> MailspaceEventInput {
    MailspaceEventInput {
        command: event.command.into(),
        event_type: event.event_type.into(),
        actor_identity: None,
        account: request.account.clone(),
        message_id: message_id.into(),
        content_id: content_id.into(),
        from_role: None,
        to_role: Some(request.local_role.clone()),
        from_identity: None,
        to_identity: Some(request.account.clone()),
        subject: event.subject.into(),
        note: event.note.map(String::from),
    }
}
