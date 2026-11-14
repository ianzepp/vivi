//! The pump page's data: health (as the overview shows it) and what the
//! pump recorded in `pump.db`, read-only.
//!
//! Every read is a named query from `queries.rs`. A table the store lacks
//! (an older pump store) reads as empty rather than failing the page.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, ToSql};
use vivi_error::VivariumError;

use super::config;
use super::overview::{PumpView, pump_view};
use super::pump_db::PumpRead;
use super::store::{self, has_table};
use crate::queries::{
    PUMP_CONTROL, PUMP_EXCEPTIONS, PUMP_GATE_FAILURES, PUMP_GATES, PUMP_RECENT_STEP_TASKS,
    PUMP_RUNG_RUNS, PUMP_UNIT_COUNTS, PUMP_UNIT_EVENTS, Query,
};

/// Unit events listed.
pub const EVENT_ROWS: i64 = 50;
/// Gates listed.
pub const GATE_ROWS: i64 = 10;
/// Exceptions listed.
pub const EXCEPTION_ROWS: i64 = 20;
/// Recent flow steps whose rung runs are read.
pub const RUNG_TASKS: i64 = 40;
/// Rung runs listed.
pub const RUNG_ROWS: usize = 50;
/// Gate failures read for the gates listed.
const GATE_FAILURE_ROWS: i64 = 500;

/// One `unit_events` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitEvent {
    pub unit: String,
    pub attempt: i64,
    pub model: Option<String>,
    pub why: String,
    pub detail: String,
    pub at: i64,
}

/// One gate and its failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    pub id: i64,
    pub tip: String,
    pub at: i64,
    pub merges: i64,
    /// Failing name and kind.
    pub failures: Vec<(String, String)>,
}

/// One `exceptions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpException {
    pub at: i64,
    pub kind: String,
    pub unit: String,
    pub text: String,
}

/// One `rung_runs` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RungRun {
    pub task: String,
    pub rung: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub rc: Option<i64>,
}

/// What the pump store holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpData {
    /// Mode and whether shutdown was requested, from `pump_control`.
    pub control: Option<(String, bool)>,
    /// Status and count, in status order.
    pub units: Vec<(String, i64)>,
    pub events: Vec<UnitEvent>,
    pub gates: Vec<Gate>,
    pub exceptions: Vec<PumpException>,
    pub rungs: Vec<RungRun>,
}

/// Whether the store could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreState {
    /// The project has no `[pump]` table.
    NoPump,
    /// Configured, but there is nothing to read: no store yet, or one the
    /// console may not open (a WAL store with no live writer).
    Unavailable(String),
    Ready(Box<PumpData>),
}

/// The pump page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpPage {
    pub now: i64,
    pub health: PumpView,
    pub store: StoreState,
}

/// Read the pump page of the project at `root`.
///
/// # Errors
/// Returns an error when the project config cannot be read.
pub fn load(root: &Path, now: i64) -> Result<PumpPage, VivariumError> {
    let config = config::load(root)?;
    let health = pump_view(root, &config);
    let store = if config.pump.is_none() {
        StoreState::NoPump
    } else {
        match PumpRead::open(root) {
            Ok(Some(read)) => match read.read(read_data) {
                Ok(data) => StoreState::Ready(Box::new(data)),
                Err(error) => StoreState::Unavailable(error.to_string()),
            },
            Ok(None) => StoreState::Unavailable("The pump has no store yet.".into()),
            Err(error) => StoreState::Unavailable(error.to_string()),
        }
    };
    Ok(PumpPage { now, health, store })
}

fn read_data(conn: &Connection) -> Result<PumpData, VivariumError> {
    let gates = read_gates(conn)?;
    Ok(PumpData {
        control: read_control(conn)?,
        units: read_units(conn)?,
        events: read_events(conn)?,
        gates,
        exceptions: read_exceptions(conn)?,
        rungs: read_rungs(conn)?,
    })
}

/// Run `query` with `params`, mapping each row; an absent table gives none.
fn rows<T>(
    conn: &Connection,
    table: &str,
    query: &Query,
    params: &[&dyn ToSql],
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, VivariumError> {
    if !has_table(conn, table)? {
        return Ok(Vec::new());
    }
    store::rows(conn, query, params, map)
}

fn read_control(conn: &Connection) -> Result<Option<(String, bool)>, VivariumError> {
    let mut found = rows(conn, "pump_control", &PUMP_CONTROL, &[], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0))
    })?;
    Ok(found.pop())
}

fn read_units(conn: &Connection) -> Result<Vec<(String, i64)>, VivariumError> {
    rows(conn, "units", &PUMP_UNIT_COUNTS, &[&""], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })
}

fn read_events(conn: &Connection) -> Result<Vec<UnitEvent>, VivariumError> {
    rows(
        conn,
        "unit_events",
        &PUMP_UNIT_EVENTS,
        &[&0, &EVENT_ROWS],
        |row| {
            Ok(UnitEvent {
                unit: row.get(1)?,
                attempt: row.get(2)?,
                model: row.get(3)?,
                why: row.get(4)?,
                detail: row.get(5)?,
                at: row.get(6)?,
            })
        },
    )
}

fn read_gates(conn: &Connection) -> Result<Vec<Gate>, VivariumError> {
    let mut gates = rows(conn, "gates", &PUMP_GATES, &[&0, &GATE_ROWS], |row| {
        Ok(Gate {
            id: row.get(0)?,
            tip: row.get(1)?,
            at: row.get(2)?,
            merges: row.get(3)?,
            failures: Vec::new(),
        })
    })?;
    let (Some(oldest), Some(newest)) = (
        gates.iter().map(|gate| gate.id).min(),
        gates.iter().map(|gate| gate.id).max(),
    ) else {
        return Ok(gates);
    };
    let failures = rows(
        conn,
        "gate_failures",
        &PUMP_GATE_FAILURES,
        &[&oldest, &newest, &GATE_FAILURE_ROWS],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (row.get::<_, String>(1)?, row.get::<_, String>(2)?),
            ))
        },
    )?;
    for (id, failure) in failures {
        if let Some(gate) = gates.iter_mut().find(|gate| gate.id == id) {
            gate.failures.push(failure);
        }
    }
    Ok(gates)
}

fn read_exceptions(conn: &Connection) -> Result<Vec<PumpException>, VivariumError> {
    rows(
        conn,
        "exceptions",
        &PUMP_EXCEPTIONS,
        &[&0, &EXCEPTION_ROWS],
        |row| {
            Ok(PumpException {
                at: row.get(1)?,
                kind: row.get(2)?,
                unit: row.get(3)?,
                text: row.get(4)?,
            })
        },
    )
}

/// Rung runs of the newest flow steps' tasks, newest first.
fn read_rungs(conn: &Connection) -> Result<Vec<RungRun>, VivariumError> {
    let tasks = rows(
        conn,
        "flow_steps",
        &PUMP_RECENT_STEP_TASKS,
        &[&0, &RUNG_TASKS],
        |row| row.get::<_, String>(0),
    )?;
    let mut runs: BTreeMap<(i64, String, String), RungRun> = BTreeMap::new();
    for task in &tasks {
        for run in rows(conn, "rung_runs", &PUMP_RUNG_RUNS, &[&task], |row| {
            Ok(RungRun {
                task: row.get(0)?,
                rung: row.get(1)?,
                started_at: row.get(2)?,
                finished_at: row.get(3)?,
                rc: row.get(4)?,
            })
        })? {
            runs.insert((run.started_at, run.task.clone(), run.rung.clone()), run);
        }
    }
    Ok(runs.into_values().rev().take(RUNG_ROWS).collect())
}

#[cfg(test)]
#[path = "pump_page_test.rs"]
pub(crate) mod tests;
