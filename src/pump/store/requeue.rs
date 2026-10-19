//! Operator requeue requests queued in `pump.db`.
//!
//! A requeue request sits beside `pump_control` and `need_priority`: a CLI
//! writes it while a pump runs, and the pump reads the pending rows each tick
//! and applies them to [`State`](crate::pump::state::State). It is never part
//! of the state itself, so no state save clears it.

use rusqlite::params;

use crate::error::VivariumError;

use super::{PumpStore, db_err};

/// The `requeue_requests` table: one row per operator requeue request.
pub const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS requeue_requests (
  request_id INTEGER PRIMARY KEY AUTOINCREMENT,
  unit TEXT NOT NULL,
  attempts INTEGER,
  fresh INTEGER NOT NULL DEFAULT 0,
  refund INTEGER NOT NULL DEFAULT 0,
  reason TEXT NOT NULL DEFAULT '',
  requested_at INTEGER NOT NULL,
  applied_at INTEGER
);
";

/// One unapplied operator requeue request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequeueRequest {
    pub request_id: i64,
    pub unit: String,
    pub attempts: Option<i64>,
    pub fresh: bool,
    pub refund: bool,
    pub reason: String,
}

impl PumpStore {
    /// Queue one requeue request for the pump to apply on its next tick.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn add_requeue_request(
        &mut self,
        unit: &str,
        attempts: Option<i64>,
        fresh: bool,
        refund: bool,
        reason: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO requeue_requests
                 (unit, attempts, fresh, refund, reason, requested_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    unit,
                    attempts,
                    fresh,
                    refund,
                    reason,
                    i64::try_from(at).unwrap_or(i64::MAX)
                ],
            )
            .map_err(db_err("write requeue request"))?;
        Ok(())
    }

    /// Every requeue request the pump has not applied yet, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub fn pending_requeue_requests(&self) -> Result<Vec<RequeueRequest>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, unit, attempts, fresh, refund, reason
                 FROM requeue_requests
                 WHERE applied_at IS NULL
                 ORDER BY request_id",
            )
            .map_err(db_err("prepare requeue requests"))?;
        stmt.query_map([], |row| {
            Ok(RequeueRequest {
                request_id: row.get(0)?,
                unit: row.get(1)?,
                attempts: row.get(2)?,
                fresh: row.get(3)?,
                refund: row.get(4)?,
                reason: row.get(5)?,
            })
        })
        .map_err(db_err("read requeue requests"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read requeue request row"))
    }

    /// Mark one request applied so later ticks ignore it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be updated.
    pub fn mark_requeue_applied(&mut self, request_id: i64, at: u64) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE requeue_requests SET applied_at = ?2
                 WHERE request_id = ?1 AND applied_at IS NULL",
                params![request_id, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("apply requeue request"))?;
        Ok(())
    }
}
