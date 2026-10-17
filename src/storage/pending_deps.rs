//! Dependency references a held work letter names that this mailspace does
//! not hold yet.
//!
//! The record lives in the store's own `storage_metadata` table as one JSON
//! map (`message_id` -> references), so it exists in a mailspace with no
//! `[sync]` and never appears as item metadata. It is a cache of what the
//! letters' `X-Vivi-Depends-On` headers say: `graph audit --repair` rebuilds
//! it, so losing it loses nothing a later audit cannot restore.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::{Storage, VivariumError, params};

const PENDING_KEY: &str = "pending_dependencies";

/// Message id -> the dependency references it still waits for.
pub type PendingDependencies = BTreeMap<String, Vec<String>>;

impl Storage {
    /// Every held letter that still waits for a dependency to arrive.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the read fails.
    pub fn pending_dependencies(&self) -> Result<PendingDependencies, VivariumError> {
        read_pending(&self.conn.borrow())
    }

    /// Replace the references `message_id` waits for; an empty list clears
    /// its record.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] if the read or write fails.
    pub fn set_pending_dependencies(
        &self,
        message_id: &str,
        references: &[String],
    ) -> Result<(), VivariumError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| VivariumError::Other(format!("failed to begin pending write: {e}")))?;
        let mut pending = read_pending(&tx)?;
        let changed = if references.is_empty() {
            pending.remove(message_id).is_some()
        } else {
            pending
                .insert(message_id.to_string(), references.to_vec())
                .as_deref()
                != Some(references)
        };
        if changed {
            let raw = serde_json::to_string(&pending).map_err(|e| {
                VivariumError::Other(format!("failed to encode pending dependencies: {e}"))
            })?;
            tx.execute(
                "INSERT OR REPLACE INTO storage_metadata (key, value) VALUES (?1, ?2)",
                params![PENDING_KEY, raw],
            )
            .map_err(|e| VivariumError::Other(format!("failed to write pending: {e}")))?;
        }
        tx.commit()
            .map_err(|e| VivariumError::Other(format!("failed to commit pending write: {e}")))
    }
}

/// An unreadable record is the empty one: it is a cache, and the audit
/// rebuilds it.
fn read_pending(conn: &Connection) -> Result<PendingDependencies, VivariumError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM storage_metadata WHERE key = ?1",
            params![PENDING_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| VivariumError::Other(format!("failed to read pending dependencies: {e}")))?;
    Ok(match raw.map(|raw| serde_json::from_str(&raw)) {
        Some(Ok(pending)) => pending,
        _ => PendingDependencies::new(),
    })
}
