use std::collections::BTreeMap;

use super::{Storage, VivariumError, params};

impl Storage {
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
                    "INSERT INTO event_deferrals (message_id, until)
                     VALUES (?1, ?2)
                     ON CONFLICT(message_id) DO UPDATE SET
                       until = excluded.until",
                    params![message_id, until],
                ),
                None => self.conn.borrow_mut().execute(
                    "DELETE FROM event_deferrals WHERE message_id = ?1",
                    params![message_id],
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
                "DELETE FROM event_deferrals
                 WHERE message_id IN
                   (SELECT message_id FROM messages WHERE content_id = ?1)",
                params![content_id],
            )
            .map(|_| ())
            .map_err(|e| VivariumError::Other(format!("failed to clear the event deferral: {e}")))
    }

    /// The earliest clock reading at which a skipped event letter held by
    /// the item `content_id` begins to count, or `None` when it holds
    /// none. Only live, unabsorbed rows count — a sealed item takes no
    /// events.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn event_deferral_until(&self, content_id: &str) -> Result<Option<i64>, VivariumError> {
        self.conn
            .borrow()
            .query_row(
                "SELECT MIN(d.until) FROM event_deferrals d
                 JOIN messages m ON m.message_id = d.message_id
                 WHERE m.content_id = ?1 AND m.deleted_at IS NULL AND m.absorbed_at IS NULL",
                params![content_id],
                |row| row.get(0),
            )
            .map_err(|e| {
                VivariumError::Other(format!("failed to query the deferral of an item: {e}"))
            })
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
                "SELECT m.content_id, d.until FROM event_deferrals d
                 JOIN messages m ON m.message_id = d.message_id
                 WHERE m.deleted_at IS NULL AND m.absorbed_at IS NULL",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare deferral query: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| VivariumError::Other(format!("failed to query deferrals: {e}")))?;
        let mut earliest: BTreeMap<String, i64> = BTreeMap::new();
        for row in rows {
            let (content_id, until) =
                row.map_err(|e| VivariumError::Other(format!("failed to read deferral row: {e}")))?;
            earliest
                .entry(content_id)
                .and_modify(|held| *held = (*held).min(until))
                .or_insert(until);
        }
        Ok(earliest.into_iter().collect())
    }
}
