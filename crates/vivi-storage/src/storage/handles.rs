use std::cell::Ref;

use super::{
    HashMap, OptionalExtension, Storage, StoredMessageView, VivariumError, params, short_handle,
    short_handle_map,
};

/// Every active message's id and short handle, built in one pass.
///
/// Explicitly preloaded by bulk commands such as `vivi step`, which resolves
/// one token per ready node. One-off resolution uses primary-key ranges and
/// decoration computes the fixed prefix directly; neither builds this index.
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
    /// short handle -> every message id carrying it.
    by_handle: HashMap<String, Vec<String>>,
}

impl HandleIndex {
    /// Build an index over `ids`, stamped with the freshness version sampled
    /// before `ids` was read. The stamp travels only in memory; it is never
    /// persisted or compared across connections.
    fn new(ids: Vec<String>, data_version: i64) -> Self {
        let mut by_handle: HashMap<String, Vec<String>> = HashMap::new();
        for message_id in &ids {
            let handle = short_handle(message_id);
            by_handle
                .entry(handle.clone())
                .or_default()
                .push(message_id.clone());
        }
        Self {
            data_version,
            ids,
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

    /// Opt into the cached full index for a command that resolves many tokens.
    /// One-off resolution and display never build this index.
    ///
    /// # Errors
    /// Returns an error if the index cannot be loaded.
    pub fn preload_handle_index(&self) -> Result<(), VivariumError> {
        drop(self.handle_index()?);
        Ok(())
    }

    /// Resolve a full id, short handle, or id prefix to a canonical message id.
    /// Uses primary-key ranges unless a bulk caller explicitly preloaded the index.
    ///
    /// # Errors
    /// Returns an error for an ambiguous or missing token, or a failed query.
    pub fn resolve_message_token(&self, token: &str) -> Result<String, VivariumError> {
        if let Some(id) = self.message_by_id_exact(token)? {
            return Ok(id);
        }
        let cached = self.handle_index.borrow().is_some();
        let matches = if cached {
            self.indexed_token_matches(token)?
        } else {
            self.direct_token_matches(token, None)?
        };
        if let Some(id) = resolve_prefix_matches(token, matches, false)? {
            return Ok(id);
        }
        resolve_content_matches(token, &self.content_prefix_matches(token)?, false)
    }

    fn indexed_token_matches(&self, token: &str) -> Result<(Vec<String>, bool), VivariumError> {
        let index = self.handle_index()?;
        if let Some(matches) = index.by_handle.get(token) {
            return Ok((matches.clone(), true));
        }
        let (prefix, handle) = token_prefix(token);
        let matches: Vec<String> = index
            .ids
            .iter()
            .filter(|id| id.starts_with(&prefix))
            .cloned()
            .collect();
        if !matches.is_empty() {
            return Ok((matches, handle));
        }
        Ok((
            index
                .ids
                .iter()
                .filter(|id| id.starts_with(token))
                .cloned()
                .collect(),
            false,
        ))
    }

    fn direct_token_matches(
        &self,
        token: &str,
        accounts: Option<&[String]>,
    ) -> Result<(Vec<String>, bool), VivariumError> {
        let (prefix, handle) = token_prefix(token);
        let mut matches = self.message_id_prefix_matches(&prefix, accounts, i64::MAX)?;
        if accounts.is_some() && handle {
            // Scoped resolution historically considers both legacy id prefixes
            // and native bases together, rather than preferring the native one.
            matches.extend(self.message_id_prefix_matches(token, accounts, i64::MAX)?);
            matches.sort();
            matches.dedup();
            return Ok((matches, true));
        }
        if !matches.is_empty() || !handle {
            return Ok((matches, handle));
        }
        Ok((
            self.message_id_prefix_matches(token, accounts, i64::MAX)?,
            false,
        ))
    }

    /// Compute the fixed-width handle of an active message, or return the id
    /// unchanged if it is missing/deleted. Collisions remain fixed-width and
    /// are refused by resolution; no whole-mailbox index is needed to display.
    ///
    /// # Errors
    /// Returns an error if the exact-id query fails.
    pub fn display_handle(&self, message_id: &str) -> Result<String, VivariumError> {
        Ok(if self.message_by_id_exact(message_id)?.is_some() {
            short_handle(message_id)
        } else {
            message_id.to_string()
        })
    }

    /// Whether an active message id starts with this literal prefix.
    /// Unlike token resolution, this does not fall back to content ids.
    ///
    /// # Errors
    /// Returns an error if the primary-key range query fails.
    pub fn message_id_prefix_exists(&self, prefix: &str) -> Result<bool, VivariumError> {
        Ok(!self.message_id_prefix_matches(prefix, None, 1)?.is_empty())
    }

    fn message_id_prefix_matches(
        &self,
        prefix: &str,
        accounts: Option<&[String]>,
        limit: i64,
    ) -> Result<Vec<String>, VivariumError> {
        if accounts.is_some_and(<[String]>::is_empty) {
            return Ok(Vec::new());
        }
        let end = prefix_end(prefix);
        let scope = accounts.map_or_else(String::new, |accounts| {
            format!(
                " AND account IN ({})",
                sql_placeholders(accounts.len(), 4).join(",")
            )
        });
        let upper = if end.is_some() {
            "message_id < ?2"
        } else {
            "?2 IS NULL"
        };
        let sql = format!(
            "SELECT message_id FROM messages INDEXED BY sqlite_autoindex_messages_1
             WHERE message_id >= ?1 AND {upper} AND deleted_at IS NULL{scope}
             ORDER BY message_id LIMIT ?3"
        );
        let mut values: Vec<&dyn rusqlite::types::ToSql> = vec![&prefix, &end, &limit];
        if let Some(accounts) = accounts {
            values.extend(to_sql_params(accounts));
        }
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            VivariumError::Other(format!("failed to prepare message id prefix query: {e}"))
        })?;
        let rows = stmt
            .query_map(values.as_slice(), |row| row.get(0))
            .map_err(|e| VivariumError::Other(format!("failed to query message id prefix: {e}")))?;
        rows.map(|row| {
            row.map_err(|e| VivariumError::Other(format!("failed to read message id prefix: {e}")))
        })
        .collect()
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
        if let Some(id) = resolve_prefix_matches(
            token,
            self.direct_token_matches(token, Some(accounts))?,
            true,
        )? {
            return Ok(id);
        }
        resolve_content_matches(
            token,
            &self.content_prefix_matches_for_accounts(token, accounts)?,
            true,
        )
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

    pub(super) fn decorate_handles(mut messages: Vec<StoredMessageView>) -> Vec<StoredMessageView> {
        for message in &mut messages {
            message.handle = short_handle(&message.message_id);
        }
        messages
    }
}

fn token_prefix(token: &str) -> (String, bool) {
    if token.starts_with("msg_") {
        (token.to_string(), false)
    } else {
        (format!("msg_{token}"), true)
    }
}

// Increment a Unicode scalar, not a byte; `%` and `_` stay literal.
// An empty/all-maximal prefix has no upper bound.
fn prefix_end(prefix: &str) -> Option<String> {
    for (offset, ch) in prefix.char_indices().rev() {
        let next = u32::from(ch) + 1;
        if let Some(next) = char::from_u32(if next == 0xd800 { 0xe000 } else { next }) {
            return Some(format!("{}{next}", &prefix[..offset]));
        }
    }
    None
}

fn resolve_prefix_matches(
    token: &str,
    (matches, handle): (Vec<String>, bool),
    scoped: bool,
) -> Result<Option<String>, VivariumError> {
    let scope = if scoped { " for identity" } else { "" };
    let kind = if handle || scoped {
        "handle"
    } else {
        "message_id prefix"
    };
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        n => Err(VivariumError::Message(format!(
            "ambiguous {kind} '{token}'{scope}; matches {n} messages"
        ))),
    }
}

fn resolve_content_matches(
    token: &str,
    matches: &[String],
    scoped: bool,
) -> Result<String, VivariumError> {
    let scope = if scoped { " for identity" } else { "" };
    match matches.len() {
        1 => Ok(matches[0].clone()),
        n if n > 1 => Err(VivariumError::Message(format!(
            "ambiguous content_id prefix '{token}'{scope}; matches {n} messages"
        ))),
        _ => Err(VivariumError::Message(format!(
            "message not found{scope}: {token}"
        ))),
    }
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
