use std::collections::HashMap;

use super::{
    OptionalExtension, Storage, StoredMessageView, VivariumError, message_query, params,
    raw_stored_message_from_row,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MailspaceLink {
    pub child_content_id: String,
    pub parent_content_id: String,
    pub source: String,
}

impl Storage {
    /// Link a child content blob to a parent mailspace message.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the content IDs are identical, the
    /// source is unsupported, or the database insert fails.
    pub fn link_mailspace_content(
        &self,
        child_content_id: &str,
        parent_content_id: &str,
        source: &str,
    ) -> Result<(), VivariumError> {
        if child_content_id == parent_content_id {
            return Err(VivariumError::Message(
                "a mailspace message cannot reply to itself".into(),
            ));
        }
        if !matches!(source, "captured" | "inferred" | "source") {
            return Err(VivariumError::Message(format!(
                "unsupported mailspace link source '{source}'"
            )));
        }
        self.conn
            .execute(
                "INSERT INTO mailspace_links (child_content_id, parent_content_id, source)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(child_content_id) DO UPDATE SET
                   parent_content_id = CASE
                     WHEN mailspace_links.source = 'captured' AND excluded.source = 'inferred'
                       THEN mailspace_links.parent_content_id
                     ELSE excluded.parent_content_id
                   END,
                   source = CASE
                     WHEN mailspace_links.source = 'captured' AND excluded.source = 'inferred'
                       THEN mailspace_links.source
                     ELSE excluded.source
                   END",
                params![child_content_id, parent_content_id, source],
            )
            .map_err(|e| VivariumError::Other(format!("failed to store mailspace link: {e}")))?;
        Ok(())
    }

    /// Look up the parent link for a child content ID.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn mailspace_link_for_child(
        &self,
        child_content_id: &str,
    ) -> Result<Option<MailspaceLink>, VivariumError> {
        self.conn
            .query_row(
                "SELECT child_content_id, parent_content_id, source
                 FROM mailspace_links WHERE child_content_id = ?1",
                params![child_content_id],
                mailspace_link_from_row,
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to read mailspace link: {e}")))
    }

    /// The content ids that answer `parent_content_id`: every child the link
    /// table records for it, oldest id first.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn reply_child_contents(
        &self,
        parent_content_id: &str,
    ) -> Result<Vec<String>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT child_content_id FROM mailspace_links
                 WHERE parent_content_id = ?1 ORDER BY child_content_id",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare reply query: {e}")))?;
        let rows = stmt
            .query_map(params![parent_content_id], |row| row.get(0))
            .map_err(|e| VivariumError::Other(format!("failed to query replies: {e}")))?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read reply row: {e}")))
        })
        .collect()
    }

    /// Batch-load links for multiple child `content_ids` in a single query.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn list_mailspace_links_for_children(
        &self,
        content_ids: &[String],
    ) -> Result<HashMap<String, MailspaceLink>, VivariumError> {
        if content_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders: Vec<_> = content_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT child_content_id, parent_content_id, source
             FROM mailspace_links
             WHERE child_content_id IN ({})
             ORDER BY child_content_id",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!("failed to prepare batch link query: {e}"))
        })?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = content_ids
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params_refs.as_slice(), mailspace_link_from_row)
            .map_err(|e| VivariumError::Other(format!("failed to query batch links: {e}")))?;
        let mut map = HashMap::new();
        for row in rows {
            let link = row
                .map_err(|e| VivariumError::Other(format!("failed to read batch link row: {e}")))?;
            map.insert(link.child_content_id.clone(), link);
        }
        Ok(map)
    }

    /// List all mailspace links.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn list_mailspace_links(&self) -> Result<Vec<MailspaceLink>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT child_content_id, parent_content_id, source
                 FROM mailspace_links ORDER BY child_content_id",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare link query: {e}")))?;
        let rows = stmt
            .query_map([], mailspace_link_from_row)
            .map_err(|e| VivariumError::Other(format!("failed to query mailspace links: {e}")))?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read mailspace link row: {e}")))
        })
        .collect()
    }

    /// Look up a message by content hash.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// resolution fails.
    pub fn message_by_content_id(
        &self,
        content_id: &str,
    ) -> Result<Option<StoredMessageView>, VivariumError> {
        let mut message = self
            .conn
            .query_row(
                &message_query("WHERE m.content_id = ?1 AND m.deleted_at IS NULL ORDER BY m.message_id LIMIT 1"),
                params![content_id],
                raw_stored_message_from_row,
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to read content message: {e}")))?;
        if let Some(message) = &mut message {
            message.handle = self.display_handle(&message.message_id)?;
        }
        Ok(message)
    }
}

fn mailspace_link_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailspaceLink> {
    Ok(MailspaceLink {
        child_content_id: row.get(0)?,
        parent_content_id: row.get(1)?,
        source: row.get(2)?,
    })
}
