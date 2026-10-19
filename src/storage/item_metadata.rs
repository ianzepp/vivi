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

    /// Retrieve all visible metadata key-value pairs for a message. Empty
    /// branch/commit values are internal reset markers and are omitted.
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
        let mut metadata = BTreeMap::new();
        for row in rows {
            let (key, value) =
                row.map_err(|e| VivariumError::Other(format!("failed to read metadata row: {e}")))?;
            if matches!(key.as_str(), "branch" | "commit") && value.is_empty() {
                continue;
            }
            metadata.insert(key, value);
        }
        Ok(metadata)
    }
}
