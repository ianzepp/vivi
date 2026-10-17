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
            .borrow_mut()
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
            .borrow()
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
        let conn = self.conn.borrow();
        let mut stmt = conn
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

    /// The links of one thread: every edge whose endpoint is reachable from
    /// `content_id`, without loading the whole link table. Each child has at
    /// most one parent (the child column is the primary key), so a thread is
    /// the ancestor chain plus every descendant of it.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn links_for_thread(
        &self,
        content_id: &str,
    ) -> Result<std::collections::BTreeMap<String, MailspaceLink>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "WITH RECURSIVE
                 up(cid) AS (
                   SELECT ?1
                   UNION
                   SELECT parent_content_id FROM mailspace_links, up
                    WHERE child_content_id = up.cid
                 ),
                 comp(cid) AS (
                   SELECT cid FROM up
                   UNION
                   SELECT child_content_id FROM mailspace_links, comp
                    WHERE parent_content_id = comp.cid
                 )
                 SELECT l.child_content_id, l.parent_content_id, l.source
                 FROM mailspace_links l
                 WHERE l.child_content_id IN (SELECT cid FROM comp)
                    OR l.parent_content_id IN (SELECT cid FROM comp)
                 ORDER BY l.child_content_id",
            )
            .map_err(|e| {
                VivariumError::Other(format!("failed to prepare thread link query: {e}"))
            })?;
        let rows = stmt
            .query_map(params![content_id], mailspace_link_from_row)
            .map_err(|e| VivariumError::Other(format!("failed to query thread links: {e}")))?;
        let mut map = std::collections::BTreeMap::new();
        for row in rows {
            let link = row.map_err(|e| {
                VivariumError::Other(format!("failed to read thread link row: {e}"))
            })?;
            map.insert(link.child_content_id.clone(), link);
        }
        Ok(map)
    }

    /// Batch-load links for multiple child `content_ids`.
    ///
    /// IDs are queried in chunks of [`SQL_PARAMETER_CHUNK`] so one statement
    /// never exceeds SQLite's bound-parameter limit.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn list_mailspace_links_for_children(
        &self,
        content_ids: &[String],
    ) -> Result<HashMap<String, MailspaceLink>, VivariumError> {
        let mut map = HashMap::new();
        for chunk in content_ids.chunks(super::SQL_PARAMETER_CHUNK) {
            for link in self.links_for_child_chunk(chunk)? {
                map.insert(link.child_content_id.clone(), link);
            }
        }
        Ok(map)
    }

    /// Load links for one chunk of child `content_ids` in a single query.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    fn links_for_child_chunk(&self, chunk: &[String]) -> Result<Vec<MailspaceLink>, VivariumError> {
        let placeholders = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>();
        let sql = format!(
            "SELECT child_content_id, parent_content_id, source
             FROM mailspace_links
             WHERE child_content_id IN ({})
             ORDER BY child_content_id",
            placeholders.join(",")
        );
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!("failed to prepare batch link query: {e}"))
        })?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params_refs.as_slice(), mailspace_link_from_row)
            .map_err(|e| VivariumError::Other(format!("failed to query batch links: {e}")))?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read batch link row: {e}")))
        })
        .collect()
    }

    /// List all mailspace links.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn list_mailspace_links(&self) -> Result<Vec<MailspaceLink>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
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
        let mut message = self.conn.borrow().query_row(
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
