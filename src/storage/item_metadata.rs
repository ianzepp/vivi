use std::collections::BTreeMap;

use super::{Storage, Utc, VivariumError, params};

impl Storage {
    /// Set metadata key-value pairs for a message.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the token resolution or database
    /// insert/update fails.
    pub fn set_item_metadata(
        &self,
        message_id: &str,
        metadata: &BTreeMap<String, String>,
    ) -> Result<(), VivariumError> {
        let resolved = self.resolve_message_token(message_id)?;
        super::mutate::reject_if_absorbed(&self.conn.borrow(), &resolved, message_id)?;
        let now = Utc::now().to_rfc3339();
        for (key, value) in metadata {
            self.conn
                .borrow_mut()
                .execute(
                    "INSERT INTO mailspace_item_metadata
                       (message_id, key, value, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(message_id, key) DO UPDATE SET
                       value = excluded.value,
                       updated_at = excluded.updated_at",
                    params![resolved, key, value, now],
                )
                .map_err(|e| VivariumError::Other(format!("failed to set item metadata: {e}")))?;
        }
        Ok(())
    }

    /// Retrieve all metadata key-value pairs for a message.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the token resolution or database
    /// query fails.
    pub fn item_metadata(
        &self,
        message_id: &str,
    ) -> Result<BTreeMap<String, String>, VivariumError> {
        let resolved = self.resolve_message_token(message_id)?;
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT key, value FROM mailspace_item_metadata
                 WHERE message_id = ?1 ORDER BY key",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare metadata query: {e}")))?;
        let rows = stmt
            .query_map(params![resolved], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| VivariumError::Other(format!("failed to query metadata: {e}")))?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read metadata row: {e}")))
        })
        .collect()
    }

    /// Mark `message_ids` with the clock reading (Unix microseconds) at
    /// which an event letter this mailspace skipped as dated past its clock
    /// begins to count, or clear the mark with `None`. The mark is the
    /// receiver's own bookkeeping, not item metadata a person sets: it
    /// bypasses the absorbed guard, and only unabsorbed rows are asked for.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database write fails.
    pub fn set_event_deferral(
        &self,
        message_ids: &[String],
        until: Option<i64>,
    ) -> Result<(), VivariumError> {
        for message_id in message_ids {
            let result = match until {
                Some(until) => self.conn.borrow_mut().execute(
                    "INSERT INTO mailspace_item_metadata
                       (message_id, key, value, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(message_id, key) DO UPDATE SET
                       value = excluded.value,
                       updated_at = excluded.updated_at",
                    params![
                        message_id,
                        EVENT_DEFERRED_KEY,
                        until.to_string(),
                        Utc::now().to_rfc3339()
                    ],
                ),
                None => self.conn.borrow_mut().execute(
                    "DELETE FROM mailspace_item_metadata WHERE message_id = ?1 AND key = ?2",
                    params![message_id, EVENT_DEFERRED_KEY],
                ),
            };
            result.map_err(|e| {
                VivariumError::Other(format!("failed to write the event deferral: {e}"))
            })?;
        }
        Ok(())
    }

    /// Clear the deferral mark of every row of `content_id`.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database write fails.
    pub fn clear_event_deferral(&self, content_id: &str) -> Result<(), VivariumError> {
        self.conn
            .borrow_mut()
            .execute(
                "DELETE FROM mailspace_item_metadata
                 WHERE key = ?1 AND message_id IN
                   (SELECT message_id FROM messages WHERE content_id = ?2)",
                params![EVENT_DEFERRED_KEY, content_id],
            )
            .map(|_| ())
            .map_err(|e| VivariumError::Other(format!("failed to clear the event deferral: {e}")))
    }

    /// Every item holding a skipped event letter: its content id and the
    /// earliest clock reading at which one of them begins to count. Only
    /// live, unabsorbed rows count — a sealed item takes no events.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn deferred_event_contents(&self) -> Result<Vec<(String, i64)>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT m.content_id, md.value FROM mailspace_item_metadata md
                 JOIN messages m ON m.message_id = md.message_id
                 WHERE md.key = ?1 AND m.deleted_at IS NULL AND m.absorbed_at IS NULL",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare deferral query: {e}")))?;
        let rows = stmt
            .query_map(params![EVENT_DEFERRED_KEY], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| VivariumError::Other(format!("failed to query deferrals: {e}")))?;
        let mut earliest: BTreeMap<String, i64> = BTreeMap::new();
        for row in rows {
            let (content_id, value) =
                row.map_err(|e| VivariumError::Other(format!("failed to read deferral row: {e}")))?;
            let Ok(until) = value.parse::<i64>() else {
                continue;
            };
            earliest
                .entry(content_id)
                .and_modify(|held| *held = (*held).min(until))
                .or_insert(until);
        }
        Ok(earliest.into_iter().collect())
    }
}

/// The metadata key that marks an item holding an event letter skipped as
/// dated past the receiver's clock.
const EVENT_DEFERRED_KEY: &str = "event_deferred_until";
