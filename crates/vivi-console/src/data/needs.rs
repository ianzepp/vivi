//! The needs list's data.
//!
//! A need is a node of the mailspace's backlog work graph, keyed by the need's
//! handle (the first eight characters of its message id). Its attempts are the
//! `slot_runs` rows whose `need` equals that handle: the pump stamps its jobs
//! with the need handle it keys its own tables by, and a task sent from a need
//! carries the need handle as its `source_handle`. The flow position comes from
//! `pump.db` when a pump is running; its tables are keyed by the same handle.

use std::path::Path;

use chrono::DateTime;
use rusqlite::{Connection, Row, ToSql};
use vivi_error::VivariumError;

use super::SLOT_RUNS_TABLE;
use super::need_flow::{self, FlowPos, PumpState};
use super::runs::{RunRow, run_from_row};
use super::store::{ReadStore, db_err, has_table};
use crate::queries::{NEED_ATTEMPT_COUNT, NEED_ATTEMPTS, NEEDS_LIST, Query};

/// Needs read for one list page, newest first.
pub const LIST_LIMIT: i64 = 300;

/// Which needs the list reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    Open,
    Done,
    Any,
}

impl StateFilter {
    /// The filter a query-string word names; anything else is `Open`.
    #[must_use]
    pub fn parse(word: &str) -> Self {
        match word {
            "done" => Self::Done,
            "any" => Self::Any,
            _ => Self::Open,
        }
    }

    /// The word the query string uses.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Any => "any",
        }
    }

    fn states(self) -> [&'static str; 3] {
        match self {
            Self::Open => ["open", "active", "open"],
            Self::Done => ["done", "done", "done"],
            Self::Any => ["open", "active", "done"],
        }
    }
}

/// One need as the backlog graph holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedNode {
    pub handle: String,
    pub subject: String,
    pub state: String,
    /// When the need was filed (epoch seconds), if the node has a readable stamp.
    pub created_at: Option<i64>,
}

/// A stored date (RFC 3339 or RFC 2822) as epoch seconds.
pub(super) fn epoch_of(text: &str) -> Option<i64> {
    match DateTime::parse_from_rfc3339(text) {
        Ok(when) => Some(when.timestamp()),
        Err(_) => match DateTime::parse_from_rfc2822(text) {
            Ok(when) => Some(when.timestamp()),
            Err(_) => None,
        },
    }
}

pub(super) fn node_from_row(row: &Row<'_>) -> rusqlite::Result<NeedNode> {
    let created: Option<String> = row.get(3)?;
    Ok(NeedNode {
        handle: row.get(0)?,
        subject: row.get(1)?,
        state: row.get(2)?,
        created_at: created.as_deref().and_then(epoch_of),
    })
}

/// One run of a need's work in a pool slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRow {
    pub run: RunRow,
    pub landing_reason: Option<String>,
}

fn attempt_from_row(row: &Row<'_>) -> rusqlite::Result<AttemptRow> {
    Ok(AttemptRow {
        run: run_from_row(row)?,
        landing_reason: row.get(16)?,
    })
}

/// A need's attempts: how many, and the newest ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempts {
    pub count: i64,
    pub newest: Vec<AttemptRow>,
}

/// The attempts of the need with handle `handle`, newest `limit`; `None` when
/// the store has no slot index.
///
/// # Errors
/// Returns an error when a query fails.
pub fn attempts_of(
    conn: &Connection,
    handle: &str,
    limit: i64,
) -> Result<Option<Attempts>, VivariumError> {
    if !has_table(conn, SLOT_RUNS_TABLE)? {
        return Ok(None);
    }
    let context = "read a need's attempts";
    let count = conn
        .prepare_cached(NEED_ATTEMPT_COUNT.sql)
        .and_then(|mut stmt| stmt.query_row([handle], |row| row.get(0)))
        .map_err(db_err(context))?;
    let mut stmt = conn
        .prepare_cached(NEED_ATTEMPTS.sql)
        .map_err(db_err(context))?;
    let params: [&dyn ToSql; 2] = [&handle, &limit];
    let newest = stmt
        .query_map(params, attempt_from_row)
        .map_err(db_err(context))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err(context))?;
    Ok(Some(Attempts { count, newest }))
}

/// One row of the needs list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedRow {
    pub node: NeedNode,
    /// `None` when the store has no slot index.
    pub attempts: Option<Attempts>,
    /// The flow position, when the pump has a run for the need.
    pub flow: Option<FlowPos>,
}

impl NeedRow {
    /// The kind of work: the flow run's, else the newest attempt's.
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        if let Some(flow) = &self.flow {
            return Some(flow.run.kind.as_str());
        }
        self.attempts.as_ref()?.newest.first()?.run.kind.as_deref()
    }

    /// The current step's node, if the need is in the flow.
    #[must_use]
    pub fn step(&self) -> Option<&str> {
        self.flow.as_ref()?.step.as_ref().map(|s| s.node.as_str())
    }
}

/// Everything the needs list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedsData {
    pub now: i64,
    pub rows: Vec<NeedRow>,
    /// The needs read hit [`LIST_LIMIT`]: older ones are not shown.
    pub truncated: bool,
    pub pump: PumpState<()>,
}

/// Load the needs list; `None` when the project has no mailspace store.
///
/// # Errors
/// Returns an error when the mailspace store cannot be read.
pub fn load(
    root: &Path,
    now: i64,
    filter: StateFilter,
) -> Result<Option<NeedsData>, VivariumError> {
    let Some(store) = ReadStore::open(root)? else {
        return Ok(None);
    };
    let pump = need_flow::open_pump(root);
    let status = pump.status();
    let pump_read = match &pump {
        PumpState::Ready(read) => Some(read),
        _ => None,
    };
    let (rows, truncated) = store.read(|conn| match pump_read {
        Some(read) => read.read(|pconn| list_rows(conn, Some(pconn), filter)),
        None => list_rows(conn, None, filter),
    })?;
    Ok(Some(NeedsData {
        now,
        rows,
        truncated,
        pump: status,
    }))
}

fn list_rows(
    conn: &Connection,
    pump: Option<&Connection>,
    filter: StateFilter,
) -> Result<(Vec<NeedRow>, bool), VivariumError> {
    let nodes = need_nodes(conn, &NEEDS_LIST, filter)?;
    let truncated = i64::try_from(nodes.len()).unwrap_or(i64::MAX) >= LIST_LIMIT;
    let flows = match pump {
        Some(pconn) => need_flow::has_flow_tables(pconn)?,
        None => false,
    };
    let mut rows = Vec::with_capacity(nodes.len());
    for node in nodes {
        let attempts = attempts_of(conn, &node.handle, 1)?;
        let flow = match pump {
            Some(pconn) if flows => need_flow::position(pconn, Some(conn), &node.handle)?,
            _ => None,
        };
        rows.push(NeedRow {
            node,
            attempts,
            flow,
        });
    }
    Ok((rows, truncated))
}

fn need_nodes(
    conn: &Connection,
    query: &Query,
    filter: StateFilter,
) -> Result<Vec<NeedNode>, VivariumError> {
    if !has_table(conn, "work_graph_nodes")? {
        return Ok(Vec::new());
    }
    let context = "read the needs";
    let [a, b, c] = filter.states();
    let params: [&dyn ToSql; 4] = [&a, &b, &c, &LIST_LIMIT];
    let mut stmt = conn.prepare_cached(query.sql).map_err(db_err(context))?;
    stmt.query_map(params, node_from_row)
        .map_err(db_err(context))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err(context))
}

#[cfg(test)]
#[path = "needs_seed_test.rs"]
pub(crate) mod seed;
#[cfg(test)]
#[path = "needs_test.rs"]
mod tests;
