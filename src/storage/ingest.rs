use super::events::append_event_tx;
use super::metadata::ParsedMetadata;
use super::{
    MailspaceEventInput, MessageIngestRequest, RemoteBindingInput, Storage, StoredMessage, Utc,
    VivariumError, blob_relpath, fallback_message_id, params, parse_metadata,
    remote_bound_message_id, sha256_hex, write_blob_if_absent,
};
use rusqlite::Transaction;

impl Storage {
    /// Ingest a single message, inserting or updating blob, metadata, message,
    /// and remote binding rows atomically.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database transaction, blob write,
    /// or metadata parsing fails.
    pub fn ingest_message(
        &mut self,
        request: &MessageIngestRequest,
        data: &[u8],
    ) -> Result<StoredMessage, VivariumError> {
        let content_id = sha256_hex(data);
        let blob_relpath = blob_relpath(&content_id);
        let blob_abspath = self.mail_root.join(&blob_relpath);
        let created_blob = write_blob_if_absent(&blob_abspath, data)?;
        let metadata = parse_metadata(data);
        let message_id = ingest_message_id(request, &content_id);
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction().map_err(|e| {
            VivariumError::Other(format!("failed to open storage transaction: {e}"))
        })?;
        upsert_blob_row(&tx, &content_id, &blob_relpath, data.len(), &metadata, &now)?;
        upsert_metadata_row(&tx, &content_id, &metadata)?;
        upsert_message_row(&tx, request, &message_id, &content_id, &now)?;
        upsert_remote_binding(&tx, &message_id, request.remote.as_ref(), &now)?;
        tx.commit().map_err(|e| {
            VivariumError::Other(format!("failed to commit storage transaction: {e}"))
        })?;
        self.invalidate_handle_index();
        Ok(StoredMessage {
            message_id,
            content_id,
            blob_relpath,
            created_blob,
        })
    }

    /// Atomically ingest a single blob for multiple recipients and log a
    /// delivery event for each, all within one `SQLite` transaction. If any
    /// message row or event insert fails, the entire batch rolls back — no
    /// partial recipient-visible state survives.
    ///
    /// The blob file is content-addressed: on rollback it may persist as an
    /// orphan, but dedup by content hash means it is harmless and will be
    /// reused on the next successful delivery of the same content.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database transaction, blob write,
    /// metadata parsing, or event logging fails.
    #[allow(clippy::too_many_arguments)]
    pub fn deliver_raw_batch(
        &mut self,
        requests: &[MessageIngestRequest],
        data: &[u8],
        event_command: &str,
        event_type: &str,
        event_to_role: &str,
        event_subject: &str,
    ) -> Result<Vec<StoredMessage>, VivariumError> {
        self.deliver_raw_batch_inner(
            requests,
            data,
            event_command,
            event_type,
            event_to_role,
            event_subject,
            None,
        )
    }

    /// Test-only variant that injects a failure after `fail_after` message
    /// rows are processed within the transaction, proving that the batch
    /// rolls back completely (no partial rows or events).
    ///
    /// # Errors
    /// Returns an error if the database transaction fails or after the
    /// injected failure point.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn deliver_raw_batch_fail_after(
        &mut self,
        requests: &[MessageIngestRequest],
        data: &[u8],
        event_command: &str,
        event_type: &str,
        event_to_role: &str,
        event_subject: &str,
        fail_after: usize,
    ) -> Result<Vec<StoredMessage>, VivariumError> {
        self.deliver_raw_batch_inner(
            requests,
            data,
            event_command,
            event_type,
            event_to_role,
            event_subject,
            Some(fail_after),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver_raw_batch_inner(
        &mut self,
        requests: &[MessageIngestRequest],
        data: &[u8],
        event_command: &str,
        event_type: &str,
        event_to_role: &str,
        event_subject: &str,
        fail_after: Option<usize>,
    ) -> Result<Vec<StoredMessage>, VivariumError> {
        let content_id = sha256_hex(data);
        let blob_relpath = blob_relpath(&content_id);
        let blob_abspath = self.mail_root.join(&blob_relpath);
        let created_blob = write_blob_if_absent(&blob_abspath, data)?;
        let metadata = parse_metadata(data);
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction().map_err(|e| {
            VivariumError::Other(format!("failed to open delivery transaction: {e}"))
        })?;
        upsert_blob_row(&tx, &content_id, &blob_relpath, data.len(), &metadata, &now)?;
        upsert_metadata_row(&tx, &content_id, &metadata)?;
        let mut stored = Vec::with_capacity(requests.len());
        for (index, request) in requests.iter().enumerate() {
            if fail_after.is_some_and(|n| index >= n) {
                return Err(VivariumError::Other(
                    "injected batch failure for testing".into(),
                ));
            }
            let message_id = ingest_message_id(request, &content_id);
            upsert_message_row(&tx, request, &message_id, &content_id, &now)?;
            upsert_remote_binding(&tx, &message_id, request.remote.as_ref(), &now)?;
            let event = MailspaceEventInput {
                command: event_command.into(),
                event_type: event_type.into(),
                actor_identity: None,
                account: request.account.clone(),
                message_id: message_id.clone(),
                content_id: content_id.clone(),
                from_role: None,
                to_role: Some(event_to_role.into()),
                from_identity: None,
                to_identity: Some(request.account.clone()),
                subject: event_subject.into(),
                note: None,
            };
            append_event_tx(&tx, &event, &now)?;
            stored.push(StoredMessage {
                message_id,
                content_id: content_id.clone(),
                blob_relpath: blob_relpath.clone(),
                created_blob,
            });
        }
        tx.commit().map_err(|e| {
            VivariumError::Other(format!("failed to commit delivery transaction: {e}"))
        })?;
        self.invalidate_handle_index();
        Ok(stored)
    }
}

pub(super) fn ingest_message_id(request: &MessageIngestRequest, content_id: &str) -> String {
    request.message_id_hint.clone().unwrap_or_else(|| {
        request.remote.as_ref().map_or_else(
            || fallback_message_id(request, content_id),
            |remote| {
                remote_bound_message_id(&request.account, &request.local_role, content_id, remote)
            },
        )
    })
}

pub(super) fn upsert_blob_row(
    tx: &Transaction<'_>,
    content_id: &str,
    blob_relpath: &str,
    byte_size: usize,
    metadata: &ParsedMetadata,
    now: &str,
) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO blobs (content_id, blob_relpath, byte_size, rfc_message_id, parsed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(content_id) DO UPDATE SET
           blob_relpath = excluded.blob_relpath,
           byte_size = excluded.byte_size,
           rfc_message_id = COALESCE(excluded.rfc_message_id, blobs.rfc_message_id)",
        params![
            content_id,
            blob_relpath,
            i64::try_from(byte_size).unwrap_or(i64::MAX),
            metadata.normalized_message_id,
            now,
        ],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to upsert blob row: {e}")))
}

pub(super) fn upsert_metadata_row(
    tx: &Transaction<'_>,
    content_id: &str,
    metadata: &ParsedMetadata,
) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO message_metadata (
           content_id, date, from_addr, to_addr, cc_addr, bcc_addr, subject, normalized_message_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(content_id) DO UPDATE SET
           date = excluded.date,
           from_addr = excluded.from_addr,
           to_addr = excluded.to_addr,
           cc_addr = excluded.cc_addr,
           bcc_addr = excluded.bcc_addr,
           subject = excluded.subject,
           normalized_message_id = excluded.normalized_message_id",
        params![
            content_id,
            metadata.date,
            metadata.from_addr,
            metadata.to_addr,
            metadata.cc_addr,
            metadata.bcc_addr,
            metadata.subject,
            metadata.normalized_message_id,
        ],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to upsert message metadata: {e}")))
}

pub(super) fn upsert_message_row(
    tx: &Transaction<'_>,
    request: &MessageIngestRequest,
    message_id: &str,
    content_id: &str,
    now: &str,
) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO messages (
           message_id, account, content_id, local_role, read_state, starred,
           draft_state, discovered_at, updated_at, deleted_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, NULL)
         ON CONFLICT(message_id) DO UPDATE SET
           content_id = excluded.content_id,
           local_role = excluded.local_role,
           read_state = excluded.read_state,
           starred = excluded.starred,
           updated_at = excluded.updated_at,
           deleted_at = NULL",
        params![
            message_id,
            request.account,
            content_id,
            request.local_role,
            i32::from(request.read_state),
            i32::from(request.starred),
            now,
            now,
        ],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to upsert message row: {e}")))
}

fn upsert_remote_binding(
    tx: &Transaction<'_>,
    message_id: &str,
    remote: Option<&RemoteBindingInput>,
    now: &str,
) -> Result<(), VivariumError> {
    let Some(remote) = remote else {
        return Ok(());
    };
    tx.execute(
        "INSERT INTO remote_bindings (
           message_id, account, provider, remote_mailbox, remote_uid,
           remote_uidvalidity, last_verified_at, stale
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
         ON CONFLICT(message_id) DO UPDATE SET
           account = excluded.account,
           provider = excluded.provider,
           remote_mailbox = excluded.remote_mailbox,
           remote_uid = excluded.remote_uid,
           remote_uidvalidity = excluded.remote_uidvalidity,
           last_verified_at = excluded.last_verified_at,
           stale = 0",
        params![
            message_id,
            remote.account,
            remote.provider,
            remote.remote_mailbox,
            remote.remote_uid,
            remote.remote_uidvalidity,
            now,
        ],
    )
    .map(|_| ())
    .map_err(|e| VivariumError::Other(format!("failed to upsert remote binding: {e}")))
}
