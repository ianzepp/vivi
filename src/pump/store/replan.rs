//! Operator replan requests queued in `pump.db`.

use rusqlite::params;

use crate::error::VivariumError;

use super::{PumpStore, db_err};

/// The `replan_requests` table: one row per operator replan request.
pub const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS replan_requests (
  request_id INTEGER PRIMARY KEY AUTOINCREMENT,
  need TEXT NOT NULL,
  force INTEGER NOT NULL DEFAULT 0,
  requested_at INTEGER NOT NULL,
  applied_at INTEGER
);
";

/// One unapplied operator replan request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplanRequest {
    pub request_id: i64,
    pub need: String,
    pub force: bool,
}

impl PumpStore {
    /// Queue one replan request for the pump to apply on its next tick.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn add_replan_request(
        &mut self,
        need: &str,
        force: bool,
        at: u64,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO replan_requests (need, force, requested_at)
                 VALUES (?1, ?2, ?3)",
                params![need, force, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("write replan request"))?;
        Ok(())
    }

    /// Every replan request the pump has not applied yet, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub fn pending_replan_requests(&self) -> Result<Vec<ReplanRequest>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, need, force FROM replan_requests
                 WHERE applied_at IS NULL
                 ORDER BY request_id",
            )
            .map_err(db_err("prepare replan requests"))?;
        stmt.query_map([], |row| {
            Ok(ReplanRequest {
                request_id: row.get(0)?,
                need: row.get(1)?,
                force: row.get(2)?,
            })
        })
        .map_err(db_err("read replan requests"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read replan request row"))
    }

    /// Mark one request applied so later ticks ignore it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be updated.
    pub fn mark_replan_applied(&mut self, request_id: i64, at: u64) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE replan_requests SET applied_at = ?2
                 WHERE request_id = ?1 AND applied_at IS NULL",
                params![request_id, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("mark replan request applied"))?;
        Ok(())
    }
}
