//! Owner rows: which pump job executes one logical step execution.

use rusqlite::{OptionalExtension, Row, params};

use crate::error::VivariumError;

use crate::pump::store::{PumpStore, db_err, from_u64};

const OWNER_COLUMNS: &str =
    "need, run, task, node, phase, seq, job, inputs_json, transient_json, allowance";

/// One logical execution of a step phase and the pump job that owns it.
///
/// The key is (need, run, task, node, phase) plus the execution sequence: a
/// re-run of the same phase appends the next sequence instead of rewriting
/// history. `job` is the current pump job. `inputs_json` and
/// `transient_json` are opaque JSON stamps kept with the execution:
/// the saved execution inputs and its transient usage. `allowance` is retry
/// allowance already granted to the execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionOwner {
    pub need: String,
    pub run: u64,
    pub task: String,
    pub node: String,
    pub phase: String,
    pub seq: u64,
    pub job: Option<String>,
    pub inputs_json: String,
    pub transient_json: String,
    pub allowance: u64,
}

fn read_owner(row: &Row<'_>) -> rusqlite::Result<ExecutionOwner> {
    Ok(ExecutionOwner {
        need: row.get(0)?,
        run: from_u64(row.get(1)?),
        task: row.get(2)?,
        node: row.get(3)?,
        phase: row.get(4)?,
        seq: from_u64(row.get(5)?),
        job: row.get(6)?,
        inputs_json: row.get(7)?,
        transient_json: row.get(8)?,
        allowance: from_u64(row.get(9)?),
    })
}

impl PumpStore {
    /// Register an execution's owner row. Registration is idempotent: a row
    /// that already exists is left exactly as it is, so re-registering never
    /// resets a granted allowance or a bound pump job.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub(crate) fn register_execution(&self, owner: &ExecutionOwner) -> Result<(), VivariumError> {
        self.conn
            .execute(
                &format!(
                    "INSERT INTO recovery_owners ({OWNER_COLUMNS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(need, run, task, node, phase, seq) DO NOTHING"
                ),
                params![
                    owner.need,
                    i64::try_from(owner.run).unwrap_or(0),
                    owner.task,
                    owner.node,
                    owner.phase,
                    i64::try_from(owner.seq).unwrap_or(0),
                    owner.job,
                    owner.inputs_json,
                    owner.transient_json,
                    i64::try_from(owner.allowance).unwrap_or(0),
                ],
            )
            .map_err(db_err("write recovery owner"))?;
        Ok(())
    }

    /// Every execution of one (need, run, task, node, phase) owner, oldest
    /// sequence first. History stays addressable: a re-run is a new row.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub(crate) fn lookup_executions(
        &self,
        need: &str,
        run: u64,
        task: &str,
        node: &str,
        phase: &str,
    ) -> Result<Vec<ExecutionOwner>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {OWNER_COLUMNS} FROM recovery_owners
                 WHERE need = ?1 AND run = ?2 AND task = ?3 AND node = ?4 AND phase = ?5
                 ORDER BY seq"
            ))
            .map_err(db_err("prepare recovery owners"))?;
        let rows = stmt
            .query_map(
                params![need, i64::try_from(run).unwrap_or(0), task, node, phase],
                read_owner,
            )
            .map_err(db_err("read recovery owners"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read recovery owner row"))
    }

    /// The current owner of one (need, run, task, node, phase): its newest
    /// execution, or `None` when the phase never ran.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub(crate) fn current_execution(
        &self,
        need: &str,
        run: u64,
        task: &str,
        node: &str,
        phase: &str,
    ) -> Result<Option<ExecutionOwner>, VivariumError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {OWNER_COLUMNS} FROM recovery_owners
                     WHERE need = ?1 AND run = ?2 AND task = ?3 AND node = ?4 AND phase = ?5
                     ORDER BY seq DESC LIMIT 1"
                ),
                params![need, i64::try_from(run).unwrap_or(0), task, node, phase],
                read_owner,
            )
            .optional()
            .map_err(db_err("read recovery owner"))
    }

    /// Bind the current pump job to the execution named by `owner`'s key;
    /// `None` unbinds. The pump job identity is the pump's own, never the
    /// pool's.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub(crate) fn set_execution_job(
        &self,
        owner: &ExecutionOwner,
        job: Option<&str>,
    ) -> Result<(), VivariumError> {
        self.write_owner_field(
            "job = ?7",
            params![
                owner.need,
                i64::try_from(owner.run).unwrap_or(0),
                owner.task,
                owner.node,
                owner.phase,
                i64::try_from(owner.seq).unwrap_or(0),
                job,
            ],
        )
    }

    /// Grant retry allowance to the execution named by `owner`'s key. The
    /// value is whatever the caller's policy grants; registration never
    /// resets it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be written.
    pub(crate) fn set_execution_allowance(
        &self,
        owner: &ExecutionOwner,
        allowance: u64,
    ) -> Result<(), VivariumError> {
        self.write_owner_field(
            "allowance = ?7",
            params![
                owner.need,
                i64::try_from(owner.run).unwrap_or(0),
                owner.task,
                owner.node,
                owner.phase,
                i64::try_from(owner.seq).unwrap_or(0),
                i64::try_from(allowance).unwrap_or(i64::MAX),
            ],
        )
    }

    fn write_owner_field(
        &self,
        assignment: &str,
        bound: impl rusqlite::Params,
    ) -> Result<(), VivariumError> {
        let sql = format!(
            "UPDATE recovery_owners SET {assignment}
             WHERE need = ?1 AND run = ?2 AND task = ?3 AND node = ?4 AND phase = ?5 AND seq = ?6"
        );
        self.conn
            .execute(&sql, bound)
            .map_err(db_err("write recovery owner field"))?;
        Ok(())
    }
}
