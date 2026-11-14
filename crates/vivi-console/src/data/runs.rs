//! Reads over the `slot_runs` index. Every query is a named
//! [`crate::queries::Query`]; this module only binds and decodes.

use rusqlite::{Connection, Row};
use vivi_error::VivariumError;

use super::store::{db_err, rows};
use crate::queries::{
    DISPATCHED_SINCE, FAILURES_SINCE, HOURLY_SINCE, OPEN_RUNS, OUTCOMES_SINCE, RECENT_RUNS,
    SLOT_HISTORY,
};

/// One `slot_runs` row, the columns the pages show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub job: String,
    pub task_message_id: String,
    pub need: Option<String>,
    pub pool: String,
    pub slot: String,
    pub machine: String,
    pub model: String,
    pub kind: Option<String>,
    pub dispatched_at: i64,
    pub budget_s: Option<i64>,
    pub collected_at: Option<i64>,
    pub outcome: Option<String>,
    pub agent_rc: Option<i64>,
    pub check_rc: Option<i64>,
    pub job_s: Option<i64>,
    pub landing: Option<String>,
}

impl RunRow {
    /// True for a collected run that failed: its outcome is not `finished`
    /// or an exit code is non-zero. Mirrors the SQL in `queries.rs`.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.outcome.as_deref().unwrap_or("") != "finished"
            || self.agent_rc.unwrap_or(0) != 0
            || self.check_rc.unwrap_or(0) != 0
    }
}

/// A slot seen in the recent past, with its newest run's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownSlot {
    pub pool: String,
    pub slot: String,
    pub machine: String,
    pub last_dispatched_at: i64,
    pub last_outcome: Option<String>,
    pub last_collected_at: Option<i64>,
}

/// Runs collected in the window, by outcome and failure flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeCount {
    pub outcome: String,
    pub failed: bool,
    pub count: i64,
}

/// Runs collected in one epoch hour, by failure flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HourCount {
    pub hour: i64,
    pub failed: bool,
    pub count: i64,
}

pub(super) fn run_from_row(row: &Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        job: row.get(0)?,
        task_message_id: row.get(1)?,
        need: row.get(2)?,
        pool: row.get(3)?,
        slot: row.get(4)?,
        machine: row.get(5)?,
        model: row.get(6)?,
        kind: row.get(7)?,
        dispatched_at: row.get(8)?,
        budget_s: row.get(9)?,
        collected_at: row.get(10)?,
        outcome: row.get(11)?,
        agent_rc: row.get(12)?,
        check_rc: row.get(13)?,
        job_s: row.get(14)?,
        landing: row.get(15)?,
    })
}

/// Runs dispatched and not yet collected, newest first, at most `limit`.
///
/// # Errors
/// Returns an error when the query fails.
pub fn open_runs(conn: &Connection, limit: i64) -> Result<Vec<RunRow>, VivariumError> {
    let mut rows = rows(conn, &OPEN_RUNS, &[&limit], run_from_row)?;
    rows.sort_by(|a, b| b.dispatched_at.cmp(&a.dispatched_at));
    Ok(rows)
}

/// How many runs were dispatched since `since` (epoch seconds).
///
/// # Errors
/// Returns an error when the query fails.
pub fn dispatched_since(conn: &Connection, since: i64) -> Result<i64, VivariumError> {
    conn.prepare_cached(DISPATCHED_SINCE.sql)
        .and_then(|mut stmt| stmt.query_row([since], |row| row.get(0)))
        .map_err(db_err("count dispatches"))
}

/// Collected runs since `since`, by outcome and failure flag.
///
/// # Errors
/// Returns an error when the query fails.
pub fn outcomes_since(conn: &Connection, since: i64) -> Result<Vec<OutcomeCount>, VivariumError> {
    rows(conn, &OUTCOMES_SINCE, &[&since], |row| {
        Ok(OutcomeCount {
            outcome: row.get(0)?,
            failed: row.get::<_, i64>(1)? != 0,
            count: row.get(2)?,
        })
    })
}

/// Collected runs since `since`, by epoch hour and failure flag.
///
/// # Errors
/// Returns an error when the query fails.
pub fn hourly_since(conn: &Connection, since: i64) -> Result<Vec<HourCount>, VivariumError> {
    rows(conn, &HOURLY_SINCE, &[&since], |row| {
        Ok(HourCount {
            hour: row.get(0)?,
            failed: row.get::<_, i64>(1)? != 0,
            count: row.get(2)?,
        })
    })
}

/// The newest failed runs collected since `since`, at most `limit`.
///
/// # Errors
/// Returns an error when the query fails.
pub fn failures_since(
    conn: &Connection,
    since: i64,
    limit: i64,
) -> Result<Vec<RunRow>, VivariumError> {
    rows(conn, &FAILURES_SINCE, &[&since, &limit], run_from_row)
}

/// The slots dispatched to since `since`, each with its newest run's machine
/// and result. Reads at most `limit` recent runs to find them.
///
/// # Errors
/// Returns an error when the query fails.
pub fn known_slots(
    conn: &Connection,
    since: i64,
    limit: i64,
) -> Result<Vec<KnownSlot>, VivariumError> {
    let recent = rows(conn, &RECENT_RUNS, &[&since, &limit], |row| {
        Ok(KnownSlot {
            pool: row.get(0)?,
            slot: row.get(1)?,
            machine: row.get(2)?,
            last_dispatched_at: row.get(3)?,
            last_outcome: row.get(4)?,
            last_collected_at: row.get(5)?,
        })
    })?;
    // Newest first, so the first row of each slot is its latest run.
    let mut seen = std::collections::HashSet::new();
    Ok(recent
        .into_iter()
        .filter(|slot| seen.insert((slot.pool.clone(), slot.slot.clone())))
        .collect())
}

/// One slot's runs, newest first, at most `limit`.
///
/// # Errors
/// Returns an error when the query fails.
pub fn slot_history(
    conn: &Connection,
    pool: &str,
    slot: &str,
    limit: i64,
) -> Result<Vec<RunRow>, VivariumError> {
    rows(conn, &SLOT_HISTORY, &[&pool, &slot, &limit], run_from_row)
}
