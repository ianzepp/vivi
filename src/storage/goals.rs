//! Goal path registry: pointers to on-disk goal documents, not freeform bodies.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use super::{Storage, VivariumError, sha256_hex};

/// One registered goal document path.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GoalRow {
    pub handle: String,
    pub path: String,
    pub label: Option<String>,
    pub registered_by: Option<String>,
    pub created_at: String,
}

/// Deterministic goal handle from normalized project-relative path.
#[must_use]
pub fn goal_handle_for_path(path: &str) -> String {
    let digest = sha256_hex(format!("goal\0{path}").as_bytes());
    format!("gol_{}", &digest[..16])
}

impl Storage {
    /// Insert a goal path registration.
    ///
    /// # Errors
    /// Returns an error when the insert fails (including duplicate path).
    pub fn goal_insert(
        &self,
        path: &str,
        label: Option<&str>,
        registered_by: Option<&str>,
    ) -> Result<GoalRow, VivariumError> {
        let handle = goal_handle_for_path(path);
        let created_at = Utc::now().to_rfc3339();
        self.conn
            .borrow_mut()
            .execute(
                "INSERT INTO mailspace_goals (handle, path, label, registered_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![handle, path, label, registered_by, created_at],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    VivariumError::Other(format!(
                        "goal path already registered: {path} (use goal list / goal show)"
                    ))
                } else {
                    VivariumError::Other(format!("failed to register goal: {e}"))
                }
            })?;
        Ok(GoalRow {
            handle,
            path: path.to_string(),
            label: label.map(str::to_string),
            registered_by: registered_by.map(str::to_string),
            created_at,
        })
    }

    /// List all registered goals, ordered by path.
    ///
    /// # Errors
    /// Returns an error when the query fails.
    pub fn goals(&self) -> Result<Vec<GoalRow>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT handle, path, label, registered_by, created_at
                 FROM mailspace_goals
                 ORDER BY path",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare goal list: {e}")))?;
        let rows = stmt
            .query_map([], map_goal_row)
            .map_err(|e| VivariumError::Other(format!("failed to list goals: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| VivariumError::Other(format!("failed to read goal row: {e}")))
    }

    /// Look up a goal by handle.
    ///
    /// # Errors
    /// Returns an error when the query fails.
    pub fn goal_by_handle(&self, handle: &str) -> Result<Option<GoalRow>, VivariumError> {
        self.conn
            .borrow()
            .query_row(
                "SELECT handle, path, label, registered_by, created_at
                 FROM mailspace_goals WHERE handle = ?1",
                params![handle],
                map_goal_row,
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to load goal by handle: {e}")))
    }

    /// Look up a goal by exact stored path.
    ///
    /// # Errors
    /// Returns an error when the query fails.
    pub fn goal_by_path(&self, path: &str) -> Result<Option<GoalRow>, VivariumError> {
        self.conn
            .borrow()
            .query_row(
                "SELECT handle, path, label, registered_by, created_at
                 FROM mailspace_goals WHERE path = ?1",
                params![path],
                map_goal_row,
            )
            .optional()
            .map_err(|e| VivariumError::Other(format!("failed to load goal by path: {e}")))
    }

    /// Resolve a handle prefix when it is unambiguous.
    ///
    /// # Errors
    /// Returns an error when the query fails or the prefix matches multiple goals.
    pub fn goal_by_handle_prefix(&self, prefix: &str) -> Result<Option<GoalRow>, VivariumError> {
        let conn = self.conn.borrow();
        let mut stmt = conn
            .prepare(
                "SELECT handle, path, label, registered_by, created_at
                 FROM mailspace_goals WHERE handle LIKE ?1 || '%'
                 ORDER BY handle",
            )
            .map_err(|e| VivariumError::Other(format!("failed to prepare goal prefix: {e}")))?;
        let rows = stmt
            .query_map(params![prefix], map_goal_row)
            .map_err(|e| VivariumError::Other(format!("failed to query goal prefix: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| VivariumError::Other(format!("failed to read goal prefix row: {e}")))?;
        match rows.len() {
            0 => Ok(None),
            1 => Ok(rows.into_iter().next()),
            _ => Err(VivariumError::Other(format!(
                "ambiguous goal handle prefix '{prefix}' matches {} goals",
                rows.len()
            ))),
        }
    }

    /// Hard-delete a goal registration by handle.
    ///
    /// # Errors
    /// Returns an error when the delete fails.
    pub fn goal_delete(&self, handle: &str) -> Result<bool, VivariumError> {
        let n = self
            .conn
            .borrow_mut()
            .execute(
                "DELETE FROM mailspace_goals WHERE handle = ?1",
                params![handle],
            )
            .map_err(|e| VivariumError::Other(format!("failed to drop goal: {e}")))?;
        Ok(n > 0)
    }
}

fn map_goal_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GoalRow> {
    Ok(GoalRow {
        handle: row.get(0)?,
        path: row.get(1)?,
        label: row.get(2)?,
        registered_by: row.get(3)?,
        created_at: row.get(4)?,
    })
}

fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                ..
            },
            _
        )
    )
}
