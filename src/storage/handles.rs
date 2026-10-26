use std::cell::Ref;

use super::{
    HashMap, OptionalExtension, Storage, StoredMessageView, VivariumError, params, short_handle,
    short_handle_map,
};

/// Every active message's id and short handle, built in one pass.
///
/// Handle resolution, handle decoration, and the token prefix scan are
/// repeated many times per command — `vivi step` resolves one token per ready
/// node. Building this once per [`Storage`] keeps those calls out of the
/// `messages` table, whose scan cost grows with the whole mailbox.
///
/// The index is stamped with the `PRAGMA data_version` sampled before its ids
/// were loaded. This connection's own commits are handled by explicit
/// invalidation (they do not advance its own `data_version`); observing a
/// different version on a later access means another connection committed,
/// and the index is rebuilt instead of served.
pub(super) struct HandleIndex {
    /// Freshness stamp: the `PRAGMA data_version` observed before `ids` was
    /// loaded. Never sampled after the load — a sample taken afterward could
    /// label stale ids as fresh, while an older stamp can only make the next
    /// access rebuild, which is safe.
    data_version: i64,
    /// Active message ids in `message_id` order.
    ids: Vec<String>,
    /// `message_id` -> short handle.
    by_message_id: HashMap<String, String>,
    /// short handle -> every message id carrying it.
    by_handle: HashMap<String, Vec<String>>,
}

impl HandleIndex {
    /// Build an index over `ids`, stamped with the freshness version sampled
    /// before `ids` was read. The stamp travels only in memory; it is never
    /// persisted or compared across connections.
    fn new(ids: Vec<String>, data_version: i64) -> Self {
        let mut by_message_id = HashMap::with_capacity(ids.len());
        let mut by_handle: HashMap<String, Vec<String>> = HashMap::new();
        for message_id in &ids {
            let handle = short_handle(message_id);
            by_handle
                .entry(handle.clone())
                .or_default()
                .push(message_id.clone());
            by_message_id.insert(message_id.clone(), handle);
        }
        Self {
            data_version,
            ids,
            by_message_id,
            by_handle,
        }
    }
}

impl Storage {
    /// Cached handle index, invalidated locally by any write and rebuilt
    /// whenever `PRAGMA data_version` shows another connection committed.
    /// A failing freshness check propagates rather than serving a possibly
    /// stale cache.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the freshness check or the
    /// active-message-ids query fails.
    fn handle_index(&self) -> Result<Ref<'_, HandleIndex>, VivariumError> {
        // Sample once: the same observation decides reuse and labels any
        // rebuilt index. Sampling before the id load means a concurrent
        // external commit can only leave the stamp stale-looking, forcing a
        // safe rebuild on the next access; a sample taken after the load
        // could stamp stale ids as fresh.
        let data_version = self.connection_data_version()?;
        {
            let cached = self.handle_index.borrow();
            if let Some(index) = cached.as_ref()
                && index.data_version == data_version
            {
                return Ref::filter_map(cached, Option::as_ref)
                    .map_err(|_| VivariumError::Other("handle index is not available".into()));
            }
        }
        let ids = self.active_message_ids()?;
        *self.handle_index.borrow_mut() = Some(HandleIndex::new(ids, data_version));
        Ref::filter_map(self.handle_index.borrow(), Option::as_ref)
            .map_err(|_| VivariumError::Other("handle index is not available".into()))
    }

    /// This connection's view of the database age (`PRAGMA data_version`).
    /// The value changes when any other connection commits and is unchanged
    /// for this connection's own commits.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the pragma query fails.
    fn connection_data_version(&self) -> Result<i64, VivariumError> {
        let conn = self.conn.borrow();
        conn.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .map_err(|e| {
                VivariumError::Other(format!("failed to read handle-index freshness: {e}"))
            })
    }

    /// Resolve a message token (full ID, short handle, or ID prefix) to a
    /// canonical message ID.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the token is ambiguous or no message
    /// matches.
    pub fn resolve_message_token(&self, token: &str) -> Result<String, VivariumError> {
        if self.message_by_id_exact(token)?.is_some() {
            return Ok(token.to_string());
        }
        let index = self.handle_index()?;
        if let Some(matches) = index.by_handle.get(token) {
            return match matches.as_slice() {
                [only] => Ok(only.clone()),
                many => Err(VivariumError::Message(format!(
                    "ambiguous handle '{token}'; matches {} messages",
                    many.len()
                ))),
            };
        }
        let id_prefix_matches: Vec<&String> = index
            .ids
            .iter()
            .filter(|message_id| message_id.starts_with(token))
            .collect();
        match id_prefix_matches.as_slice() {
            [only] => return Ok((*only).clone()),
            [] => {}
            many => {
                return Err(VivariumError::Message(format!(
                    "ambiguous message_id prefix '{token}'; matches {} messages",
                    many.len()
                )));
            }
        }
        let content_matches = self.content_prefix_matches(token)?;
        match content_matches.len() {
            1 => Ok(content_matches[0].clone()),
            n if n > 1 => Err(VivariumError::Message(format!(
                "ambiguous content_id prefix '{token}'; matches {n} messages"
            ))),
            _ => Err(VivariumError::Message(format!(
                "message not found: {token}"
            ))),
        }
    }

    /// Compute a short display handle for a single message.
    ///
    /// Results come from the cached handle index, so repeated decoration does
    /// not rescan the `messages` table. The index is invalidated on any local
    /// write that affects messages (ingest, move, flag update, delete) and
    /// rebuilt when the freshness check detects another connection's commit.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the active-message-ids query fails.
    pub fn display_handle(&self, message_id: &str) -> Result<String, VivariumError> {
        let index = self.handle_index()?;
        Ok(index
            .by_message_id
            .get(message_id)
            .cloned()
            .unwrap_or_else(|| message_id.to_string()))
    }

    /// Build the full short-handle map from the database (uncached).
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the active-message-ids query fails.
    pub fn handle_map(&self) -> Result<HashMap<String, String>, VivariumError> {
        let message_ids = self.active_message_ids()?;
        Ok(short_handle_map(&message_ids))
    }

    /// Build handle maps keyed by account.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn display_handles_for_accounts(
        &self,
        accounts: &[String],
    ) -> Result<HashMap<String, String>, VivariumError> {
        let message_ids = self.active_message_ids_for_accounts(accounts)?;
        Ok(short_handle_map(&message_ids))
    }

    /// Build handle maps grouped by scope.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the database query fails.
    pub fn display_handles_for_account_scopes(
        &self,
        scopes: &[(String, Vec<String>)],
    ) -> Result<HashMap<String, HashMap<String, String>>, VivariumError> {
        let mut accounts = Vec::new();
        let mut account_scope = HashMap::new();
        for (scope, scope_accounts) in scopes {
            for account in scope_accounts {
                if !account_scope.contains_key(account) {
                    accounts.push(account.clone());
                    account_scope.insert(account.clone(), scope.clone());
                }
            }
        }
        let mut ids_by_scope = self.active_message_ids_by_scope(&accounts, &account_scope)?;
        let mut handles_by_scope = HashMap::new();
        for (scope, _) in scopes {
            let message_ids = ids_by_scope.remove(scope).unwrap_or_default();
            handles_by_scope.insert(scope.clone(), short_handle_map(&message_ids));
        }
        Ok(handles_by_scope)
    }

    /// Resolve a message token, scoped to specific accounts.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the token is ambiguous or no message
    /// matches within the given accounts.
    pub fn resolve_message_token_for_accounts(
        &self,
        token: &str,
        accounts: &[String],
    ) -> Result<String, VivariumError> {
        if let Some(message_id) = self.message_by_id_exact_for_accounts(token, accounts)? {
            return Ok(message_id);
        }
        let message_ids = self.active_message_ids_for_accounts(accounts)?;
        let handle_matches = scoped_handle_matches(token, &message_ids);
        match handle_matches.len() {
            1 => return Ok(handle_matches[0].clone()),
            n if n > 1 => {
                return Err(VivariumError::Message(format!(
                    "ambiguous handle '{token}' for identity; matches {n} messages"
                )));
            }
            _ => {}
        }
        let content_matches = self.content_prefix_matches_for_accounts(token, accounts)?;
        match content_matches.len() {
            1 => Ok(content_matches[0].clone()),
            n if n > 1 => Err(VivariumError::Message(format!(
                "ambiguous content_id prefix '{token}' for identity; matches {n} messages"
            ))),
            _ => Err(VivariumError::Message(format!(
                "message not found for identity: {token}"
            ))),
        }
    }

    pub(super) fn decorate_handles_for_accounts(
        &self,
        mut messages: Vec<StoredMessageView>,
        accounts: &[String],
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let handle_map = self.display_handles_for_accounts(accounts)?;
        for message in &mut messages {
            message.handle = handle_map
                .get(&message.message_id)
                .cloned()
                .unwrap_or_else(|| message.message_id.clone());
        }
        Ok(messages)
    }

    fn active_message_ids(&self) -> Result<Vec<String>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT message_id FROM messages WHERE deleted_at IS NULL ORDER BY message_id")
            .map_err(|e| {
                VivariumError::Other(format!("failed to prepare message id query: {e}"))
            })?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| VivariumError::Other(format!("failed to query message ids: {e}")))?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read message id row: {e}")))
        })
        .collect()
    }

    fn active_message_ids_for_accounts(
        &self,
        accounts: &[String],
    ) -> Result<Vec<String>, VivariumError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = sql_placeholders(accounts.len(), 1);
        let sql = format!(
            "SELECT message_id FROM messages
             WHERE deleted_at IS NULL AND account IN ({})
             ORDER BY message_id",
            placeholders.join(",")
        );
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!("failed to prepare scoped message id query: {e}"))
        })?;
        let params = to_sql_params(accounts);
        let rows = stmt
            .query_map(params.as_slice(), |row| row.get::<_, String>(0))
            .map_err(|e| {
                VivariumError::Other(format!("failed to query scoped message ids: {e}"))
            })?;
        rows.map(|row| {
            row.map_err(|e| {
                VivariumError::Other(format!("failed to read scoped message id row: {e}"))
            })
        })
        .collect()
    }

    fn active_message_ids_by_scope(
        &self,
        accounts: &[String],
        account_scope: &HashMap<String, String>,
    ) -> Result<HashMap<String, Vec<String>>, VivariumError> {
        if accounts.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = sql_placeholders(accounts.len(), 1);
        let sql = format!(
            "SELECT account, message_id FROM messages
             WHERE deleted_at IS NULL AND account IN ({})
             ORDER BY account, message_id",
            placeholders.join(",")
        );
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!("failed to prepare scoped message id query: {e}"))
        })?;
        let params = to_sql_params(accounts);
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| {
                VivariumError::Other(format!("failed to query scoped message ids: {e}"))
            })?;
        let mut ids_by_scope: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let (account, message_id) = row.map_err(|e| {
                VivariumError::Other(format!("failed to read scoped message id row: {e}"))
            })?;
            if let Some(scope) = account_scope.get(&account) {
                ids_by_scope
                    .entry(scope.clone())
                    .or_default()
                    .push(message_id);
            }
        }
        Ok(ids_by_scope)
    }

    fn message_by_id_exact(&self, message_id: &str) -> Result<Option<String>, VivariumError> {
        self.conn
            .borrow()
            .query_row(
                "SELECT message_id FROM messages WHERE message_id = ?1 AND deleted_at IS NULL",
                params![message_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to read exact message id: {e}")))
    }

    fn content_prefix_matches(&self, token: &str) -> Result<Vec<String>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT message_id
                 FROM messages
                 WHERE deleted_at IS NULL AND content_id LIKE ?1
                 ORDER BY message_id",
            )
            .map_err(|e| {
                VivariumError::Other(format!("failed to prepare content prefix query: {e}"))
            })?;
        let rows = stmt
            .query_map(params![format!("{token}%")], |row| row.get::<_, String>(0))
            .map_err(|e| {
                VivariumError::Other(format!("failed to query content prefix matches: {e}"))
            })?;
        rows.map(|row| {
            row.map_err(|e| {
                VivariumError::Other(format!("failed to read content prefix match row: {e}"))
            })
        })
        .collect()
    }

    fn message_by_id_exact_for_accounts(
        &self,
        message_id: &str,
        accounts: &[String],
    ) -> Result<Option<String>, VivariumError> {
        if accounts.is_empty() {
            return Ok(None);
        }
        let placeholders = sql_placeholders(accounts.len(), 1);
        let sql = format!(
            "SELECT message_id FROM messages
             WHERE message_id = ?{}
               AND deleted_at IS NULL
               AND account IN ({})",
            accounts.len() + 1,
            placeholders.join(",")
        );
        let mut params = to_sql_params(accounts);
        params.push(&message_id);
        self.conn
            .borrow()
            .query_row(&sql, params.as_slice(), |row| row.get(0))
            .optional()
            .map_err(|e| {
                VivariumError::Other(format!("failed to read scoped exact message id: {e}"))
            })
    }

    fn content_prefix_matches_for_accounts(
        &self,
        token: &str,
        accounts: &[String],
    ) -> Result<Vec<String>, VivariumError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = sql_placeholders(accounts.len(), 2);
        let pattern = format!("{token}%");
        let sql = format!(
            "SELECT message_id
             FROM messages
             WHERE deleted_at IS NULL
               AND content_id LIKE ?1
               AND account IN ({})
             ORDER BY message_id",
            placeholders.join(",")
        );
        let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&pattern];
        params.extend(to_sql_params(accounts));
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!(
                "failed to prepare scoped content prefix query: {e}"
            ))
        })?;
        let rows = stmt
            .query_map(params.as_slice(), |row| row.get::<_, String>(0))
            .map_err(|e| {
                VivariumError::Other(format!(
                    "failed to query scoped content prefix matches: {e}"
                ))
            })?;
        rows.map(|row| {
            row.map_err(|e| {
                VivariumError::Other(format!(
                    "failed to read scoped content prefix match row: {e}"
                ))
            })
        })
        .collect()
    }

    pub(super) fn decorate_handles(
        &self,
        mut messages: Vec<StoredMessageView>,
    ) -> Result<Vec<StoredMessageView>, VivariumError> {
        let index = self.handle_index()?;
        for message in &mut messages {
            message.handle = index
                .by_message_id
                .get(&message.message_id)
                .cloned()
                .unwrap_or_else(|| message.message_id.clone());
        }
        Ok(messages)
    }
}

fn scoped_handle_matches(token: &str, message_ids: &[String]) -> Vec<String> {
    message_ids
        .iter()
        .filter(|message_id| {
            message_id.starts_with(token)
                || message_id
                    .strip_prefix("msg_")
                    .is_some_and(|handle| handle.starts_with(token))
        })
        .cloned()
        .collect()
}

fn sql_placeholders(count: usize, start: usize) -> Vec<String> {
    (start..start + count)
        .map(|index| format!("?{index}"))
        .collect()
}

fn to_sql_params(values: &[String]) -> Vec<&dyn rusqlite::types::ToSql> {
    values
        .iter()
        .map(|value| value as &dyn rusqlite::types::ToSql)
        .collect()
}

#[cfg(test)]
#[path = "handles_test.rs"]
mod tests;
