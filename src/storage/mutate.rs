use rusqlite::OptionalExtension;

use super::ingest::{ingest_message_id, upsert_blob_row, upsert_message_row, upsert_metadata_row};
use super::{
    MailspaceEventInput, MessageIngestRequest, Path, Storage, StoredMessage, Utc, VivariumError,
    blob_relpath, metadata, params, parse_metadata, sha256_hex, write_blob_if_absent,
};

pub struct MailspaceMoveWithReply<'a> {
    pub account: &'a str,
    pub message_id: &'a str,
    pub local_role: &'a str,
    pub event: &'a MailspaceEventInput,
    pub reply_requests: &'a [MessageIngestRequest],
    pub reply_data: &'a [u8],
    pub parent_content_id: &'a str,
}

pub(super) fn absorbed_error(token: &str) -> VivariumError {
    VivariumError::Message(format!("{token} is absorbed and can no longer be changed"))
}

pub(super) fn reject_if_absorbed(
    conn: &rusqlite::Connection,
    message_id: &str,
    token: &str,
) -> Result<(), VivariumError> {
    let absorbed_at: Option<String> = conn
        .query_row(
            "SELECT absorbed_at FROM messages
             WHERE message_id = ?1 AND deleted_at IS NULL",
            params![message_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| VivariumError::Other(format!("failed to read absorb state: {e}")))?
        .flatten();
    if absorbed_at.is_some() {
        return Err(absorbed_error(token));
    }
    Ok(())
}

impl Storage {
    /// Seal a message. Sets `absorbed_at` / `absorbed_by` and marks it read.
    /// Returns `true` when this call newly absorbed the row, `false` when the
    /// message was already absorbed.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the message is missing or the update fails.
    pub fn absorb_message(
        &mut self,
        account: &str,
        message_id: &str,
        absorbed_by: &str,
    ) -> Result<bool, VivariumError> {
        let now = Utc::now().to_rfc3339();
        let changed = self
            .conn
            .execute(
                "UPDATE messages
                 SET absorbed_at = ?3, absorbed_by = ?4, read_state = 1, updated_at = ?3
                 WHERE account = ?1 AND message_id = ?2
                   AND deleted_at IS NULL AND absorbed_at IS NULL",
                params![account, message_id, now, absorbed_by],
            )
            .map_err(|e| VivariumError::Other(format!("failed to absorb message: {e}")))?;
        if changed > 0 {
            self.invalidate_handle_index();
            return Ok(true);
        }
        let absorbed_at: Option<String> = self
            .conn
            .query_row(
                "SELECT absorbed_at FROM messages
                 WHERE account = ?1 AND message_id = ?2 AND deleted_at IS NULL",
                params![account, message_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to read absorb state: {e}")))?
            .flatten();
        if absorbed_at.is_some() {
            return Ok(false);
        }
        Err(VivariumError::Message(format!(
            "message not found for {account}: {message_id}"
        )))
    }

    /// Move a message to a new role and ingest reply messages in a single
    /// transaction.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the token resolution, database
    /// update, blob write, or event logging fails.
    pub fn move_message_with_reply(
        &mut self,
        request: &MailspaceMoveWithReply<'_>,
    ) -> Result<Vec<StoredMessage>, VivariumError> {
        let reply = prepare_reply_blob(&self.mail_root, request.reply_data)?;
        let resolved = self.resolve_message_token(request.message_id)?;
        reject_if_absorbed(&self.conn, &resolved, request.message_id)?;
        let tx = self.conn.transaction().map_err(|e| {
            VivariumError::Other(format!("failed to open lifecycle transaction: {e}"))
        })?;
        update_moved_message(
            &tx,
            request.account,
            &resolved,
            request.local_role,
            &reply.now,
            request.message_id,
        )?;
        let replies = ingest_reply_rows(&tx, &reply, request.reply_requests)?;
        store_reply_link(
            &tx,
            &reply.content_id,
            request.parent_content_id,
            !replies.is_empty(),
        )?;
        append_lifecycle_event(&tx, request.event, &reply.now)?;
        tx.commit().map_err(|e| {
            VivariumError::Other(format!("failed to commit lifecycle transaction: {e}"))
        })?;
        self.invalidate_handle_index();
        Ok(replies)
    }

    /// Move a message to a different local role.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the token resolution fails, the
    /// message is not found, or the database update fails.
    pub fn move_message_to_role(
        &mut self,
        account: &str,
        message_id: &str,
        local_role: &str,
    ) -> Result<(), VivariumError> {
        let resolved = self.resolve_message_token(message_id)?;
        reject_if_absorbed(&self.conn, &resolved, message_id)?;
        let now = Utc::now().to_rfc3339();
        let changed = self
            .conn
            .execute(
                "UPDATE messages
                 SET local_role = ?3, updated_at = ?4
                 WHERE account = ?1 AND message_id = ?2 AND deleted_at IS NULL",
                params![account, resolved, local_role, now],
            )
            .map_err(|e| VivariumError::Other(format!("failed to move message: {e}")))?;
        if changed == 0 {
            return Err(VivariumError::Message(format!(
                "message not found for {account}: {message_id}"
            )));
        }
        self.invalidate_handle_index();
        Ok(())
    }

    /// Update read and starred flags for a message identified by remote
    /// binding.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database update fails.
    pub fn update_remote_flags(
        &mut self,
        account: &str,
        remote_mailbox: &str,
        remote_uidvalidity: u32,
        remote_uid: u32,
        read_state: bool,
        starred: bool,
    ) -> Result<bool, VivariumError> {
        let now = Utc::now().to_rfc3339();
        let changed = self
            .conn
            .execute(
                "UPDATE messages
                 SET read_state = ?5, starred = ?6, updated_at = ?7
                 WHERE deleted_at IS NULL
                   AND message_id = (
                       SELECT message_id FROM remote_bindings
                       WHERE account = ?1
                         AND remote_mailbox = ?2
                         AND remote_uidvalidity = ?3
                         AND remote_uid = ?4
                   )",
                params![
                    account,
                    remote_mailbox,
                    remote_uidvalidity,
                    remote_uid,
                    i32::from(read_state),
                    i32::from(starred),
                    now
                ],
            )
            .map_err(|e| VivariumError::Other(format!("failed to update message flags: {e}")))?;
        if changed > 0 {
            self.invalidate_handle_index();
        }
        Ok(changed > 0)
    }

    /// Set the read state of a local (identity-owned) message without touching
    /// `starred`. `mail absorb` uses this: absorb means "read, processed, loaded
    /// into context", so the message is marked read and `unread` counts stay
    /// honest for boards and sensors that read on `read_state`.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database update fails.
    pub fn set_local_read_state(
        &mut self,
        account: &str,
        message_id: &str,
        read_state: bool,
    ) -> Result<bool, VivariumError> {
        reject_if_absorbed(&self.conn, message_id, message_id)?;
        let now = Utc::now().to_rfc3339();
        let changed = self
            .conn
            .execute(
                "UPDATE messages
                 SET read_state = ?3, updated_at = ?4
                 WHERE account = ?1 AND message_id = ?2 AND deleted_at IS NULL",
                params![account, message_id, i32::from(read_state), now],
            )
            .map_err(|e| VivariumError::Other(format!("failed to set local read state: {e}")))?;
        if changed > 0 {
            self.invalidate_handle_index();
        }
        Ok(changed > 0)
    }

    /// Mark a message as deleted (soft delete via `deleted_at` timestamp).
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database update fails.
    pub fn mark_message_deleted(
        &mut self,
        account: &str,
        message_id: &str,
    ) -> Result<bool, VivariumError> {
        reject_if_absorbed(&self.conn, message_id, message_id)?;
        let now = Utc::now().to_rfc3339();
        let changed = self
            .conn
            .execute(
                "UPDATE messages
                 SET deleted_at = ?3, updated_at = ?3
                 WHERE account = ?1 AND message_id = ?2 AND deleted_at IS NULL",
                params![account, message_id, now],
            )
            .map_err(|e| VivariumError::Other(format!("failed to mark message deleted: {e}")))?;
        if changed > 0 {
            self.invalidate_handle_index();
        }
        Ok(changed > 0)
    }
}

struct PreparedReply {
    content_id: String,
    blob_relpath: String,
    created_blob: bool,
    metadata: metadata::ParsedMetadata,
    now: String,
    byte_size: usize,
}

fn prepare_reply_blob(mail_root: &Path, data: &[u8]) -> Result<PreparedReply, VivariumError> {
    let content_id = sha256_hex(data);
    let blob_relpath = blob_relpath(&content_id);
    let created_blob = write_blob_if_absent(&mail_root.join(&blob_relpath), data)?;
    Ok(PreparedReply {
        content_id,
        blob_relpath,
        created_blob,
        metadata: parse_metadata(data),
        now: Utc::now().to_rfc3339(),
        byte_size: data.len(),
    })
}

fn update_moved_message(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    message_id: &str,
    local_role: &str,
    now: &str,
    original_token: &str,
) -> Result<(), VivariumError> {
    let changed = tx
        .execute(
            "UPDATE messages SET local_role = ?3, updated_at = ?4
             WHERE account = ?1 AND message_id = ?2 AND deleted_at IS NULL",
            params![account, message_id, local_role, now],
        )
        .map_err(|e| VivariumError::Other(format!("failed to move message: {e}")))?;
    if changed == 0 {
        return Err(VivariumError::Message(format!(
            "message not found for {account}: {original_token}"
        )));
    }
    Ok(())
}

fn ingest_reply_rows(
    tx: &rusqlite::Transaction<'_>,
    reply: &PreparedReply,
    requests: &[MessageIngestRequest],
) -> Result<Vec<StoredMessage>, VivariumError> {
    upsert_blob_row(
        tx,
        &reply.content_id,
        &reply.blob_relpath,
        reply.byte_size,
        &reply.metadata,
        &reply.now,
    )?;
    upsert_metadata_row(tx, &reply.content_id, &reply.metadata)?;
    let mut replies = Vec::new();
    for request in requests {
        let message_id = ingest_message_id(request, &reply.content_id);
        upsert_message_row(tx, request, &message_id, &reply.content_id, &reply.now)?;
        replies.push(StoredMessage {
            message_id,
            content_id: reply.content_id.clone(),
            blob_relpath: reply.blob_relpath.clone(),
            created_blob: reply.created_blob,
        });
    }
    Ok(replies)
}

fn store_reply_link(
    tx: &rusqlite::Transaction<'_>,
    child_content_id: &str,
    parent_content_id: &str,
    enabled: bool,
) -> Result<(), VivariumError> {
    if !enabled {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO mailspace_links (child_content_id, parent_content_id, source)
         VALUES (?1, ?2, 'captured')
         ON CONFLICT(child_content_id) DO UPDATE SET
           parent_content_id = excluded.parent_content_id, source = excluded.source",
        params![child_content_id, parent_content_id],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to store note link: {e}")))
}

fn append_lifecycle_event(
    tx: &rusqlite::Transaction<'_>,
    event: &MailspaceEventInput,
    now: &str,
) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO mailspace_events (
           occurred_at, command, event_type, actor_identity, account,
           message_id, content_id, from_role, to_role, from_identity,
           to_identity, subject, note
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            now,
            event.command,
            event.event_type,
            event.actor_identity,
            event.account,
            event.message_id,
            event.content_id,
            event.from_role,
            event.to_role,
            event.from_identity,
            event.to_identity,
            event.subject,
            event.note,
        ],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to append lifecycle event: {e}")))
}
