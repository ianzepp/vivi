//! Reading `slot_runs`. Every query here is answered from an index.

use rusqlite::{Row, params};
use serde::Serialize;

use super::super::{Storage, VivariumError};

/// One pool job as `slot_runs` records it. Times are epoch seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotRun {
    pub job: String,
    /// The id of the task the job was dispatched for.
    pub task_message_id: String,
    pub need: Option<String>,
    pub pool: String,
    pub slot: String,
    pub machine: String,
    pub model: String,
    pub kind: Option<String>,
    pub target: Option<String>,
    pub branch: String,
    pub start_sha: String,
    pub budget_s: Option<i64>,
    pub read_only: bool,
    pub dispatched_at: i64,
    pub collected_at: Option<i64>,
    pub outcome: Option<String>,
    pub push: Option<String>,
    pub sha: Option<String>,
    pub agent_rc: Option<i64>,
    pub agent_s: Option<i64>,
    pub check_rc: Option<i64>,
    pub check_s: Option<i64>,
    pub job_s: Option<i64>,
    pub agent_cpu_s: Option<i64>,
    pub check_cpu_s: Option<i64>,
    pub landing: Option<String>,
    pub landing_at: Option<i64>,
    pub landing_reason: Option<String>,
}

const COLUMNS: &str = "job, task_message_id, need, pool, slot, machine, model, kind, target,
    branch, start_sha, budget_s, read_only, dispatched_at, collected_at, outcome, push,
    sha, agent_rc, agent_s, check_rc, check_s, job_s, agent_cpu_s, check_cpu_s,
    landing, landing_at, landing_reason";

/// One run by job (the primary key).
const BY_JOB: &str = "SELECT {COLUMNS} FROM slot_runs WHERE job = ?1";
/// Runs not yet collected (the `collected_at` index), oldest dispatch first.
const OPEN: &str = "SELECT {COLUMNS} FROM slot_runs WHERE collected_at IS NULL
    ORDER BY dispatched_at";
/// A slot's runs, newest first (the `(pool, slot, dispatched_at)` index).
const BY_SLOT: &str = "SELECT {COLUMNS} FROM slot_runs WHERE pool = ?1 AND slot = ?2
    ORDER BY dispatched_at DESC LIMIT ?3";
/// A need's runs, newest first (the `(need, dispatched_at)` index).
const BY_NEED: &str = "SELECT {COLUMNS} FROM slot_runs WHERE need = ?1
    ORDER BY dispatched_at DESC LIMIT ?2";
/// Runs dispatched at or after a time, newest first (the `dispatched_at` index).
const RECENT: &str = "SELECT {COLUMNS} FROM slot_runs WHERE dispatched_at >= ?1
    ORDER BY dispatched_at DESC LIMIT ?2";

/// `sql` with the column list filled in.
pub(super) fn select(sql: &str) -> String {
    sql.replace("{COLUMNS}", COLUMNS)
}

/// Every query with its name and parameter count, for the plan test.
#[cfg(test)]
pub(super) fn queries() -> Vec<(&'static str, &'static str, usize)> {
    vec![
        ("by job", BY_JOB, 1),
        ("open", OPEN, 0),
        ("by slot", BY_SLOT, 3),
        ("by need", BY_NEED, 2),
        ("recent", RECENT, 2),
    ]
}

fn run_of(row: &Row<'_>) -> rusqlite::Result<SlotRun> {
    Ok(SlotRun {
        job: row.get(0)?,
        task_message_id: row.get(1)?,
        need: row.get(2)?,
        pool: row.get(3)?,
        slot: row.get(4)?,
        machine: row.get(5)?,
        model: row.get(6)?,
        kind: row.get(7)?,
        target: row.get(8)?,
        branch: row.get(9)?,
        start_sha: row.get(10)?,
        budget_s: row.get(11)?,
        read_only: row.get::<_, i64>(12)? != 0,
        dispatched_at: row.get(13)?,
        collected_at: row.get(14)?,
        outcome: row.get(15)?,
        push: row.get(16)?,
        sha: row.get(17)?,
        agent_rc: row.get(18)?,
        agent_s: row.get(19)?,
        check_rc: row.get(20)?,
        check_s: row.get(21)?,
        job_s: row.get(22)?,
        agent_cpu_s: row.get(23)?,
        check_cpu_s: row.get(24)?,
        landing: row.get(25)?,
        landing_at: row.get(26)?,
        landing_reason: row.get(27)?,
    })
}

/// SQLite's `LIMIT`, from a caller's count.
fn limit_of(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

impl Storage {
    fn query_slot_runs(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<SlotRun>, VivariumError> {
        let fail =
            |e: rusqlite::Error| VivariumError::Other(format!("failed to read slot_runs: {e}"));
        let conn = self.conn.borrow();
        let mut stmt = conn.prepare(&select(sql)).map_err(fail)?;
        let rows = stmt.query_map(params, run_of).map_err(fail)?;
        rows.collect::<Result<_, _>>().map_err(fail)
    }

    /// The run for one job.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn slot_run(&self, job: &str) -> Result<Option<SlotRun>, VivariumError> {
        Ok(self.query_slot_runs(BY_JOB, params![job])?.pop())
    }

    /// Runs dispatched and not yet collected, oldest dispatch first.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn open_slot_runs(&self) -> Result<Vec<SlotRun>, VivariumError> {
        self.query_slot_runs(OPEN, &[])
    }

    /// A slot's runs in a pool, newest dispatch first, at most `limit`.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn slot_run_history(
        &self,
        pool: &str,
        slot: &str,
        limit: usize,
    ) -> Result<Vec<SlotRun>, VivariumError> {
        self.query_slot_runs(BY_SLOT, params![pool, slot, limit_of(limit)])
    }

    /// A need's runs, newest dispatch first, at most `limit`.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn slot_runs_for_need(
        &self,
        need: &str,
        limit: usize,
    ) -> Result<Vec<SlotRun>, VivariumError> {
        self.query_slot_runs(BY_NEED, params![need, limit_of(limit)])
    }

    /// Runs dispatched at or after `since` (epoch seconds), newest first, at
    /// most `limit`.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub fn recent_slot_runs(
        &self,
        since: i64,
        limit: usize,
    ) -> Result<Vec<SlotRun>, VivariumError> {
        self.query_slot_runs(RECENT, params![since, limit_of(limit)])
    }
}
