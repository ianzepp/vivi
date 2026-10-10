use std::collections::HashMap;

use rusqlite::Transaction;

use super::{MailspaceEvent, MailspaceEventInput, Storage, Utc, VivariumError, params};

impl Storage {
    /// Append a mailspace event with the current timestamp.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database insert fails.
    pub fn append_mailspace_event(
        &self,
        event: &MailspaceEventInput,
    ) -> Result<i64, VivariumError> {
        let occurred_at = Utc::now().to_rfc3339();
        self.append_mailspace_event_at(event, &occurred_at)
    }

    /// Append a mailspace event at a specific timestamp.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database insert fails.
    pub fn append_mailspace_event_at(
        &self,
        event: &MailspaceEventInput,
        occurred_at: &str,
    ) -> Result<i64, VivariumError> {
        self.conn
            .execute(
                "INSERT INTO mailspace_events (
                   occurred_at, command, event_type, actor_identity, account,
                   message_id, content_id, from_role, to_role, from_identity,
                   to_identity, subject, note
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    occurred_at,
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
            .map_err(|e| VivariumError::Other(format!("failed to append mailspace event: {e}")))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Append every event in one transaction, sharing one timestamp.
    ///
    /// An empty slice writes nothing. `event_id` order is the insertion order,
    /// which keeps a same-timestamp batch in the caller's sequence.
    ///
    /// # Errors
    /// Returns an error if the transaction cannot be opened or any insert
    /// fails. A failure leaves the log unchanged.
    pub fn append_mailspace_events(
        &mut self,
        events: &[MailspaceEventInput],
    ) -> Result<(), VivariumError> {
        if events.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction().map_err(|error| {
            VivariumError::Other(format!("failed to open event transaction: {error}"))
        })?;
        let occurred_at = Utc::now().to_rfc3339();
        for event in events {
            append_event_tx(&tx, event, &occurred_at)?;
        }
        tx.commit().map_err(|error| {
            VivariumError::Other(format!("failed to commit event transaction: {error}"))
        })
    }

    /// Check whether an identical event already exists at the given timestamp.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn mailspace_event_exists(
        &self,
        event: &MailspaceEventInput,
        occurred_at: &str,
    ) -> Result<bool, VivariumError> {
        self.conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM mailspace_events
                   WHERE occurred_at = ?1
                     AND command = ?2
                     AND event_type = ?3
                     AND account = ?4
                     AND message_id = ?5
                     AND content_id = ?6
                     AND from_role IS ?7
                     AND to_role IS ?8
                     AND from_identity IS ?9
                     AND to_identity IS ?10
                     AND subject = ?11
                     AND note IS ?12
                 )",
                params![
                    occurred_at,
                    event.command,
                    event.event_type,
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
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(|e| VivariumError::Other(format!("failed to check mailspace event: {e}")))
    }

    /// List events for a specific message.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or token
    /// resolution fails.
    pub fn list_mailspace_events(
        &self,
        message_id: &str,
    ) -> Result<Vec<MailspaceEvent>, VivariumError> {
        let resolved = self.resolve_message_token(message_id)?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT event_id, occurred_at, command, event_type, actor_identity,
                        account, message_id, content_id, from_role, to_role,
                        from_identity, to_identity, subject, note
                 FROM mailspace_events
                 WHERE message_id = ?1
                 ORDER BY occurred_at, event_id",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare event query: {e}")))?;
        let rows = stmt
            .query_map(params![resolved], mailspace_event_from_row)
            .map_err(|e| VivariumError::Other(format!("failed to query events: {e}")))?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read event row: {e}")))
        })
        .collect()
    }

    /// Batch-load events for multiple message IDs in a single query.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn list_mailspace_events_for_messages(
        &self,
        message_ids: &[String],
    ) -> Result<HashMap<String, Vec<MailspaceEvent>>, VivariumError> {
        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders: Vec<_> = message_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT event_id, occurred_at, command, event_type, actor_identity,
                    account, message_id, content_id, from_role, to_role,
                    from_identity, to_identity, subject, note
             FROM mailspace_events
             WHERE message_id IN ({})
             ORDER BY message_id, occurred_at, event_id",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!("failed to prepare batch event query: {e}"))
        })?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = message_ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params_refs.as_slice(), mailspace_event_from_row)
            .map_err(|e| VivariumError::Other(format!("failed to query batch events: {e}")))?;
        let mut map: HashMap<String, Vec<MailspaceEvent>> = HashMap::new();
        for row in rows {
            let event = row.map_err(|e| {
                VivariumError::Other(format!("failed to read batch event row: {e}"))
            })?;
            map.entry(event.message_id.clone()).or_default().push(event);
        }
        Ok(map)
    }

    /// List events that occurred after a given event ID.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn list_mailspace_events_after(
        &self,
        event_id: i64,
    ) -> Result<Vec<MailspaceEvent>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT event_id, occurred_at, command, event_type, actor_identity,
                        account, message_id, content_id, from_role, to_role,
                        from_identity, to_identity, subject, note
                 FROM mailspace_events
                 WHERE event_id > ?1
                 ORDER BY event_id",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare event scan: {e}")))?;
        let rows = stmt
            .query_map(params![event_id], mailspace_event_from_row)
            .map_err(|e| VivariumError::Other(format!("failed to scan mailspace events: {e}")))?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read event row: {e}")))
        })
        .collect()
    }

    /// Get the largest event ID that occurred before the given timestamp.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn event_cursor_before(&self, occurred_at: &str) -> Result<i64, VivariumError> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(event_id), 0)
                 FROM mailspace_events WHERE occurred_at < ?1",
                params![occurred_at],
                |row| row.get(0),
            )
            .map_err(|e| VivariumError::Other(format!("failed to read event cursor: {e}")))
    }
}

fn mailspace_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailspaceEvent> {
    Ok(MailspaceEvent {
        event_id: row.get(0)?,
        occurred_at: row.get(1)?,
        command: row.get(2)?,
        event_type: row.get(3)?,
        actor_identity: row.get(4)?,
        account: row.get(5)?,
        message_id: row.get(6)?,
        content_id: row.get(7)?,
        from_role: row.get(8)?,
        to_role: row.get(9)?,
        from_identity: row.get(10)?,
        to_identity: row.get(11)?,
        subject: row.get(12)?,
        note: row.get(13)?,
    })
}

/// Insert a mailspace event within an existing transaction.
/// Used by batch operations that need events committed atomically
/// with message rows.
pub(super) fn append_event_tx(
    tx: &Transaction<'_>,
    event: &MailspaceEventInput,
    occurred_at: &str,
) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO mailspace_events (
           occurred_at, command, event_type, actor_identity, account,
           message_id, content_id, from_role, to_role, from_identity,
           to_identity, subject, note
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            occurred_at,
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
    .map_err(|e| VivariumError::Other(format!("failed to append mailspace event: {e}")))
}
