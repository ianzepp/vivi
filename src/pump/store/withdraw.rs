//! Operator withdraw requests queued in `pump.db`.
//!
//! Same shape as the requeue and replan queues: a CLI writes a row while a
//! pump runs, and the pump reads the pending rows each tick and applies them
//! to [`State`](crate::pump::state::State). A request is never part of the
//! state itself, so no state save clears it.

use rusqlite::params;

use crate::error::VivariumError;

use super::{PumpStore, db_err};

/// The `withdraw_requests` table: one row per operator withdraw request.
pub const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS withdraw_requests (
  request_id INTEGER PRIMARY KEY AUTOINCREMENT,
  need TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  requested_at INTEGER NOT NULL,
  applied_at INTEGER
);
";

/// One unapplied operator withdraw request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawRequest {
    pub request_id: i64,
    pub need: String,
    pub reason: String,
}

impl PumpStore {
    /// Queue one withdraw request for the pump to apply on its next tick.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn add_withdraw_request(
        &mut self,
        need: &str,
        reason: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO withdraw_requests (need, reason, requested_at)
                 VALUES (?1, ?2, ?3)",
                params![need, reason, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("write withdraw request"))?;
        Ok(())
    }

    /// Every withdraw request the pump has not applied yet, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub fn pending_withdraw_requests(&self) -> Result<Vec<WithdrawRequest>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT request_id, need, reason FROM withdraw_requests
                 WHERE applied_at IS NULL
                 ORDER BY request_id",
            )
            .map_err(db_err("prepare withdraw requests"))?;
        stmt.query_map([], |row| {
            Ok(WithdrawRequest {
                request_id: row.get(0)?,
                need: row.get(1)?,
                reason: row.get(2)?,
            })
        })
        .map_err(db_err("read withdraw requests"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read withdraw request row"))
    }

    /// Mark one request applied so later ticks ignore it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be updated.
    pub fn mark_withdraw_applied(&mut self, request_id: i64, at: u64) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE withdraw_requests SET applied_at = ?2
                 WHERE request_id = ?1 AND applied_at IS NULL",
                params![request_id, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("mark withdraw request applied"))?;
        Ok(())
    }
}
