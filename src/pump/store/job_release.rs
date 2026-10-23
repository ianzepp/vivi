//! Job-release requests of `pump.db`.
//!
//! `vivi pump requeue --job <id>` writes a row here and the running pump takes
//! it at its next tick to release a held job. Like need priority, it is
//! operator state beside `pump_control`, never part of
//! [`State`](crate::pump::state::State), so no state save touches it.

use rusqlite::{Connection, params};

use crate::error::VivariumError;

use super::{PumpStore, db_err};

const DDL: &str = "
CREATE TABLE IF NOT EXISTS job_release (
    job TEXT PRIMARY KEY,
    at INTEGER NOT NULL
);
";

pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create job release table"))
}

impl PumpStore {
    /// Ask the pump to release held job `job` at its next tick.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub fn request_job_release(&self, job: &str, at: u64) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO job_release (job, at) VALUES (?1, ?2)
                 ON CONFLICT(job) DO UPDATE SET at = ?2",
                params![job, i64::try_from(at).unwrap_or(i64::MAX)],
            )
            .map_err(db_err("write job release"))?;
        Ok(())
    }

    /// Read and delete every release request, oldest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read or cleared.
    pub fn take_job_releases(&self) -> Result<Vec<String>, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin job release"))?;
        let jobs = {
            let mut stmt = tx
                .prepare("SELECT job FROM job_release ORDER BY at, job")
                .map_err(db_err("prepare job releases"))?;
            stmt.query_map([], |row| row.get::<_, String>(0))
                .map_err(db_err("read job releases"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_err("read job release row"))?
        };
        tx.execute("DELETE FROM job_release", [])
            .map_err(db_err("clear job releases"))?;
        tx.commit().map_err(db_err("commit job release"))?;
        Ok(jobs)
    }
}

#[cfg(test)]
#[path = "job_release_test.rs"]
mod tests;
