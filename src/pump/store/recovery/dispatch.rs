//! Dispatch association rows: start evidence and accepted associations.
//!
//! The two moments are persisted separately: the evidence captured when a
//! pump job's dispatch starts, and the associations a pool accepted for it
//! afterwards. Both are keyed by the pump job id; the pool job id is its own
//! column, because a pool job's identity is not the pump job's identity. The
//! dispatch's execution fields ride in the serialized
//! [`PoolJob`](crate::pump::jobs::PoolJob) snapshot instead of duplicated
//! columns.

use rusqlite::{OptionalExtension, params};

use crate::error::VivariumError;

use crate::pump::jobs::PoolJob;
use crate::pump::store::{PumpStore, db_err, from_u64, json};

/// Evidence recorded once when a pump job's dispatch starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DispatchStart {
    /// The pump job id, never a pool job id.
    pub job: String,
    pub at: u64,
    pub evidence_json: String,
}

/// The associations accepted for one pump job: mailspace task, pool, slot
/// and the pool's own job id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DispatchAccept {
    /// The pump job id, never a pool job id.
    pub job: String,
    pub task: Option<String>,
    pub pool: Option<String>,
    pub slot: Option<String>,
    pub pool_job: Option<String>,
    pub snapshot_json: Option<String>,
}

/// The serialized [`PoolJob`] snapshot for a dispatch record.
///
/// # Errors
/// Returns [`VivariumError`] when the snapshot cannot be serialized.
pub(crate) fn snapshot_json(job: &PoolJob) -> Result<String, VivariumError> {
    json(job)
}

impl PumpStore {
    /// Record dispatch-start evidence for a pump job. The first record wins:
    /// a later write for the same job keeps the original start evidence.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub(crate) fn record_dispatch_start(&self, start: &DispatchStart) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO recovery_dispatch_starts (job, at, evidence_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(job) DO NOTHING",
                params![
                    start.job,
                    i64::try_from(start.at).unwrap_or(0),
                    start.evidence_json,
                ],
            )
            .map_err(db_err("write recovery dispatch start"))?;
        Ok(())
    }

    /// Record the associations a pool accepted for a pump job. The newest
    /// acceptance wins: a re-acceptance replaces the earlier associations.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub(crate) fn record_dispatch_accept(
        &self,
        accept: &DispatchAccept,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO recovery_dispatch_accepts
                   (job, task, pool, slot, pool_job, snapshot_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(job) DO UPDATE SET
                   task = ?2, pool = ?3, slot = ?4, pool_job = ?5, snapshot_json = ?6",
                params![
                    accept.job,
                    accept.task,
                    accept.pool,
                    accept.slot,
                    accept.pool_job,
                    accept.snapshot_json,
                ],
            )
            .map_err(db_err("write recovery dispatch accept"))?;
        Ok(())
    }

    /// Read one pump job's dispatch-start evidence, or `None` when the job
    /// never started.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub(crate) fn dispatch_start(&self, job: &str) -> Result<Option<DispatchStart>, VivariumError> {
        self.conn
            .query_row(
                "SELECT job, at, evidence_json FROM recovery_dispatch_starts WHERE job = ?1",
                [job],
                |row| {
                    Ok(DispatchStart {
                        job: row.get(0)?,
                        at: from_u64(row.get(1)?),
                        evidence_json: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(db_err("read recovery dispatch start"))
    }

    /// Read the associations accepted for one pump job, or `None` when no
    /// pool accepted it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub(crate) fn dispatch_accept(
        &self,
        job: &str,
    ) -> Result<Option<DispatchAccept>, VivariumError> {
        self.conn
            .query_row(
                "SELECT job, task, pool, slot, pool_job, snapshot_json
                 FROM recovery_dispatch_accepts WHERE job = ?1",
                [job],
                |row| {
                    Ok(DispatchAccept {
                        job: row.get(0)?,
                        task: row.get(1)?,
                        pool: row.get(2)?,
                        slot: row.get(3)?,
                        pool_job: row.get(4)?,
                        snapshot_json: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(db_err("read recovery dispatch accept"))
    }
}
