use std::collections::HashMap;

use super::{
    OptionalExtension, Storage, StoredMessageView, VivariumError, fs, message_query, params,
    raw_stored_message_from_row,
};

impl Storage {
    /// Read the raw bytes of a stored blob by content hash.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the blob row is missing or the file
    /// read fails.
    pub fn read_blob(&self, content_id: &str) -> Result<Vec<u8>, VivariumError> {
        let relpath: String = self
            .conn
            .query_row(
                "SELECT blob_relpath FROM blobs WHERE content_id = ?1",
                params![content_id],
                |row| row.get(0),
            )
            .map_err(|e| VivariumError::Other(format!("failed to read blob row: {e}")))?;
        fs::read(self.mail_root.join(relpath)).map_err(Into::into)
    }

    /// Read raw bytes from the blob store by the path a listing reported.
    ///
    /// A listing already carries the path, so this skips the query and
    /// statement preparation [`Storage::read_blob`] pays on every call.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the blob cannot be read.
    pub fn read_listed_blob(&self, blob_relpath: &str) -> Result<Vec<u8>, VivariumError> {
        fs::read(self.mail_root.join(blob_relpath)).map_err(Into::into)
    }

    /// Check whether a blob exists by content hash.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn blob_exists(&self, content_id: &str) -> Result<bool, VivariumError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM blobs WHERE content_id = ?1)",
                params![content_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(|e| VivariumError::Other(format!("failed to check blob row: {e}")))
    }

    /// Read the raw message bytes, resolving handles/prefixes first.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the token cannot be resolved, the
    /// message is not found, or the file read fails.
    pub fn read_message(&self, message_id: &str) -> Result<Vec<u8>, VivariumError> {
        let resolved = self.resolve_message_token(message_id)?;
        let Some(view) = self.message_by_id(&resolved)? else {
            return Err(VivariumError::Message(format!(
                "message not found: {message_id}"
            )));
        };
        fs::read(self.mail_root.join(view.blob_relpath)).map_err(Into::into)
    }

    /// Look up a stored message view by exact message ID.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// resolution fails.
    pub fn message_by_id(
        &self,
        message_id: &str,
    ) -> Result<Option<StoredMessageView>, VivariumError> {
        let mut message = self
            .conn
            .query_row(
                &message_query("WHERE m.message_id = ?1"),
                params![message_id],
                raw_stored_message_from_row,
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to read stored message: {e}")))?;
        if let Some(message) = &mut message {
            message.handle = self.display_handle(&message.message_id)?;
        }
        Ok(message)
    }

    /// Look up a message by content hash, account, and local role.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// resolution fails.
    pub fn message_by_content_account_role(
        &self,
        content_id: &str,
        account: &str,
        local_role: &str,
    ) -> Result<Option<StoredMessageView>, VivariumError> {
        let mut message = self
            .conn
            .query_row(
                &message_query(
                    "WHERE m.content_id = ?1
                       AND m.account = ?2
                       AND m.local_role = ?3
                       AND m.deleted_at IS NULL
                     ORDER BY m.message_id LIMIT 1",
                ),
                params![content_id, account, local_role],
                raw_stored_message_from_row,
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to read stored message: {e}")))?;
        if let Some(message) = &mut message {
            message.handle = self.display_handle(&message.message_id)?;
        }
        Ok(message)
    }

    /// List all messages in a given local role across all accounts.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// decoration fails.
    pub fn list_messages_by_role(
        &self,
        local_role: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        self.list_messages_by_query(
            "WHERE m.local_role = ?1 AND m.deleted_at IS NULL",
            params![local_role],
        )
    }

    /// List active work-role messages (tasks/needs/wants/done) updated at or
    /// after `since`. These are the backlog audit candidates: items that
    /// existed while the backlog graph did, so citizenship is expected.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// decoration fails.
    pub fn work_role_messages_updated_since(
        &self,
        since: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        self.list_messages_by_query(
            "WHERE m.local_role IN ('tasks', 'needs', 'wants', 'done') \
             AND m.deleted_at IS NULL AND m.updated_at >= ?1",
            params![since],
        )
    }

    /// List messages filtered by account and a single role.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// decoration fails.
    pub fn list_messages_by_account_role(
        &self,
        account: &str,
        local_role: &str,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        self.list_messages_by_query(
            "WHERE m.account = ?1 AND m.local_role = ?2 AND m.deleted_at IS NULL",
            params![account, local_role],
        )
    }

    /// List messages filtered by one or more accounts and one or more roles.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// decoration fails.
    pub fn list_messages_by_account_roles(
        &self,
        accounts: &[String],
        roles: &[String],
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        self.decorate_handles(self.list_messages_by_account_roles_raw(accounts, roles)?)
    }

    /// List messages filtered by accounts and roles, with handles scoped to those accounts.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// decoration fails.
    pub fn list_messages_by_account_roles_scoped(
        &self,
        accounts: &[String],
        roles: &[String],
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        self.decorate_handles_for_accounts(
            self.list_messages_by_account_roles_raw(accounts, roles)?,
            accounts,
        )
    }

    /// List messages filtered by accounts and roles without computing display handles.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn list_messages_by_account_roles_raw(
        &self,
        accounts: &[String],
        roles: &[String],
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        if accounts.is_empty() || roles.is_empty() {
            return Ok(Vec::new());
        }
        let account_placeholders: Vec<_> = accounts
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let role_offset = accounts.len();
        let role_placeholders: Vec<_> = roles
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", role_offset + i + 1))
            .collect();
        let sql = format!(
            "{} WHERE m.account IN ({}) AND m.local_role IN ({}) AND m.deleted_at IS NULL ORDER BY md.date DESC, m.message_id",
            message_query(""),
            account_placeholders.join(","),
            role_placeholders.join(","),
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!("failed to prepare account+role listing: {e}"))
        })?;
        let params: Vec<&dyn rusqlite::types::ToSql> = accounts
            .iter()
            .map(|a| a as &dyn rusqlite::types::ToSql)
            .chain(roles.iter().map(|r| r as &dyn rusqlite::types::ToSql))
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), raw_stored_message_from_row)
            .map_err(|e| {
                VivariumError::Other(format!("failed to list account+role messages: {e}"))
            })?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read message row: {e}")))
        })
        .collect()
    }

    fn list_messages_by_query(
        &self,
        where_clause: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let sql = format!(
            "{} ORDER BY md.date DESC, m.message_id",
            message_query(where_clause)
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| VivariumError::Other(format!("failed to prepare storage listing: {e}")))?;
        let rows = stmt
            .query_map(params, raw_stored_message_from_row)
            .map_err(|e| VivariumError::Other(format!("failed to list stored messages: {e}")))?;
        let messages: Result<Vec<_>, _> = rows
            .map(|row| {
                row.map_err(|e| {
                    VivariumError::Other(format!("failed to read stored message row: {e}"))
                })
            })
            .collect();
        self.decorate_handles(messages?)
    }

    /// List all non-deleted messages across all accounts.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query or handle
    /// decoration fails.
    pub fn list_messages(&self) -> Result<Vec<StoredMessageView>, VivariumError> {
        self.list_messages_by_query("WHERE m.deleted_at IS NULL", [])
    }

    /// Latest non-memo message whose `from_addr` matches any of `addresses`.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the query or handle decoration fails.
    pub fn latest_message_from_addresses(
        &self,
        addresses: &[String],
    ) -> Result<Option<StoredMessageView>, VivariumError> {
        if addresses.is_empty() {
            return Ok(None);
        }
        let numbered = (1..=addresses.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>();
        let like = (1..=addresses.len())
            .map(|index| format!("md.from_addr LIKE '%<' || ?{index} || '>%'"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let params: Vec<&dyn rusqlite::types::ToSql> = addresses
            .iter()
            .map(|a| a as &dyn rusqlite::types::ToSql)
            .collect();
        for predicate in [format!("md.from_addr IN ({})", numbered.join(",")), like] {
            let sql = format!(
                "{} WHERE m.deleted_at IS NULL AND m.local_role != 'memos' AND ({predicate}) \
                 ORDER BY md.date DESC, m.message_id LIMIT 1",
                message_query(""),
            );
            let mut stmt = self.conn.prepare(&sql).map_err(|e| {
                VivariumError::Other(format!("failed to prepare latest-from query: {e}"))
            })?;
            let found = stmt
                .query_row(params.as_slice(), raw_stored_message_from_row)
                .optional()
                .map_err(|e| {
                    VivariumError::Other(format!("failed to read latest-from message: {e}"))
                })?;
            if let Some(mut message) = found {
                message.handle = self.display_handle(&message.message_id)?;
                return Ok(Some(message));
            }
        }
        Ok(None)
    }

    /// Count non-deleted messages for an account.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn count_messages_for_account(&self, account: &str) -> Result<usize, VivariumError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE account = ?1 AND deleted_at IS NULL",
                params![account],
                |row| row.get(0),
            )
            .map_err(|e| VivariumError::Other(format!("failed to count stored messages: {e}")))
    }

    /// Count non-deleted messages for an account and role, optionally filtered by read state.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn count_messages_for_account_role(
        &self,
        account: &str,
        local_role: &str,
        read_state: Option<bool>,
    ) -> Result<usize, VivariumError> {
        let read_clause = if read_state.is_some() {
            " AND read_state = ?3"
        } else {
            ""
        };
        let absorb_clause = match local_role {
            "inbox" | "tasks" | "needs" | "wants" | "memos" => " AND absorbed_at IS NULL",
            _ => "",
        };
        let sql = format!(
            "SELECT COUNT(*) FROM messages
             WHERE account = ?1 AND local_role = ?2 AND deleted_at IS NULL{read_clause}{absorb_clause}"
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!("failed to prepare message count query: {e}"))
        })?;
        let count = if let Some(read_state) = read_state {
            stmt.query_row(params![account, local_role, i64::from(read_state)], |row| {
                row.get(0)
            })
        } else {
            stmt.query_row(params![account, local_role], |row| row.get(0))
        };
        count.map_err(|e| VivariumError::Other(format!("failed to count stored messages: {e}")))
    }

    /// Map of message identifier → byte size for a given local role.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn local_sizes_by_role(
        &self,
        local_role: &str,
    ) -> Result<HashMap<String, u64>, VivariumError> {
        let messages = self.list_messages_by_role(local_role)?;
        Ok(messages
            .into_iter()
            .map(|message| {
                let key = message
                    .remote
                    .as_ref()
                    .map(|remote| format!("{local_role}-{}", remote.remote_uid))
                    .unwrap_or(message.message_id);
                (key, message.byte_size)
            })
            .collect())
    }

    /// Build an RFC-message-id → (`remote_uid`, `byte_size`) index for a role.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn rfc_index_by_role(
        &self,
        local_role: &str,
    ) -> Result<HashMap<String, (u32, u64)>, VivariumError> {
        let messages = self.list_messages_by_role(local_role)?;
        let mut map = HashMap::new();
        for message in messages {
            let Some(rfc_message_id) = message.normalized_message_id.clone() else {
                continue;
            };
            let uid = message
                .remote
                .as_ref()
                .map(|remote| remote.remote_uid)
                .or_else(|| {
                    message
                        .message_id
                        .rsplit_once('-')
                        .and_then(|(_, uid)| uid.parse().ok())
                })
                .unwrap_or(0);
            map.insert(rfc_message_id, (uid, message.byte_size));
        }
        Ok(map)
    }
}
