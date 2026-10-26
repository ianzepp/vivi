//! Replay-safe blocked-scope resolutions of `pump.db`: one row per
//! originating source job.
//!
//! A blocked-scope answer can arrive twice: the tick may crash after the
//! store commit but before the holder dependency edges are written, and a
//! hand's corrected reply settles the same request under the same source
//! identity. The row is the durable settlement. The first apply for a source
//! job bumps the slice's request count, widens its scope, records holders
//! and scope state, and stores the resolution — all in one transaction.
//! Every later apply of the same source returns the stored row untouched:
//! no new count, no recomputation against the scopes as they stand now.
//!
//! Like the flow tables, the table is created with `IF NOT EXISTS` on every
//! open and is not part of the [`State`](crate::pump::state::State) save, so
//! `save()` and `clear()` never touch it and the schema version is not
//! bumped here.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{PumpStore, db_err, from_u32, json};
use crate::error::VivariumError;
use crate::pump::config::scope::ScopePath;
use crate::pump::flow::scope::{BlockedScope, Resolution, ScopeState};

/// A request for more paths than this goes to the slicer.
const MAX_REQUEST_PATHS: usize = 5;

/// Marks a requested-but-held path in the stored wait record (the
/// `slice_scope.holders` list); holder task handles never start with it.
pub(crate) const HELD_PATH_PREFIX: &str = "path:";

/// One stored blocked-scope resolution: the settlement of one originating
/// source job's request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeResolutionRow {
    /// The originating source job: the resolution's replay key.
    pub source_job: String,
    /// The task handle that made the request.
    pub task: String,
    /// The run the request belongs to.
    pub run: u32,
    /// The request as it was asked.
    pub request: BlockedScope,
    /// How the request was settled.
    pub resolution: Resolution,
    /// The slice's blocked-scope request count after this settlement's
    /// increment; a replayed source keeps its original count.
    pub requests: u32,
    /// The holder task handles whose dependency edges were written.
    pub holders: Vec<String>,
    /// The slice's blocked-scope state after the settlement.
    pub state: ScopeState,
    /// When the resolution was stored (unix seconds).
    pub created_at: i64,
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS scope_resolutions (
    source_job TEXT PRIMARY KEY,
    task TEXT NOT NULL,
    run INTEGER NOT NULL,
    request_json TEXT NOT NULL,
    resolution_json TEXT NOT NULL,
    requests INTEGER NOT NULL,
    holders_json TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS scope_resolutions_task ON scope_resolutions (task);
";

const COLUMNS: &str = "source_job, task, run, request_json, resolution_json, requests, \
                       holders_json, state, created_at";

/// Create the scope resolution table when it is missing.
pub(super) fn ensure_table(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create scope resolution table"))
}

impl PumpStore {
    /// Settle `task`'s blocked-scope request of `run`, keyed by the
    /// request's originating source job.
    ///
    /// The first apply for a source job bumps the slice's request count,
    /// widens its scope with the unheld paths, records holders and scope
    /// state, and stores the resolution — all in one transaction. Every
    /// later apply of the same source returns the stored row unchanged. The
    /// caller writes the [`Resolution::WaitOn`] holder dependency edges
    /// after the commit and repeats that write on replay; the mailspace
    /// dependency write is idempotent, so a crash between commit and edge
    /// write heals without a second count.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the task has no scope row, the source
    /// job was already settled for another task, or a read or write fails.
    pub fn apply_scope_resolution(
        &self,
        source_job: &str,
        task: &str,
        run: u32,
        request: &BlockedScope,
    ) -> Result<ScopeResolutionRow, VivariumError> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(db_err("begin scope resolution"))?;
        if let Some(existing) = read_by_source(&tx, source_job)? {
            if existing.task != task {
                return Err(VivariumError::Other(format!(
                    "pump.db scope_resolutions: source job {source_job} already settled \
                     for task {} instead of {task}",
                    existing.task
                )));
            }
            return Ok(existing);
        }
        let row = settle(&tx, source_job, task, run, request)?;
        tx.commit().map_err(db_err("commit scope resolution"))?;
        Ok(row)
    }

    /// The current blocked-scope request count of `task`, or `None` when the
    /// slice has no scope row. A replayed source never moves this count.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the count cannot be read.
    pub fn scope_requests(&self, task: &str) -> Result<Option<u32>, VivariumError> {
        self.conn
            .query_row(
                "SELECT scope_requests FROM slice_scope WHERE task = ?1",
                params![task],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|count| count.map(from_u32))
            .map_err(db_err("read scope requests"))
    }

    /// The resolution an originating source job produced, if it was
    /// settled already.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn scope_resolution(
        &self,
        source_job: &str,
    ) -> Result<Option<ScopeResolutionRow>, VivariumError> {
        Ok(read_by_source(&self.conn, source_job)?)
    }

    /// `task`'s outstanding resolution: the newest stored settlement still
    /// waiting on holders. Deterministic continuation reads it to find the
    /// pending dependency writes and the source job that produced the wait.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the row cannot be read or decoded.
    pub fn outstanding_scope_resolution(
        &self,
        task: &str,
    ) -> Result<Option<ScopeResolutionRow>, VivariumError> {
        Ok(rows(
            &self.conn,
            &format!(
                "SELECT {COLUMNS} FROM scope_resolutions \
                 WHERE task = ?1 AND state = 'scope-wait' \
                 ORDER BY created_at DESC, source_job DESC LIMIT 1"
            ),
            [task],
        )?
        .pop())
    }
}

/// The writes and outcome of a fresh settlement, applied in one transaction.
struct Settled {
    resolution: Resolution,
    /// Requested paths no open slice holds: added to the scope now.
    free: Vec<String>,
    /// Holder tasks whose dependency edges the caller writes after commit.
    holders: Vec<String>,
    /// The `slice_scope.holders` record: holders plus held-path evidence.
    record: Vec<String>,
    state: ScopeState,
}

/// Compute and write the settlement of a fresh request inside `tx`.
fn settle(
    tx: &Connection,
    source_job: &str,
    task: &str,
    run: u32,
    request: &BlockedScope,
) -> Result<ScopeResolutionRow, VivariumError> {
    let requests = bump_requests(tx, task)?;
    let settled = decide(tx, task, run, request, requests)?;
    if !settled.free.is_empty() {
        widen_scope(tx, task, &settled.free)?;
    }
    if settled.state == ScopeState::Wait {
        update_scope(tx, "holders", task, &json(&settled.record)?)?;
    }
    update_scope(tx, "scope_state", task, settled.state.word())?;
    let row = ScopeResolutionRow {
        source_job: source_job.to_string(),
        task: task.to_string(),
        run,
        request: request.clone(),
        resolution: settled.resolution,
        requests,
        holders: settled.holders,
        state: settled.state,
        created_at: now_unix(),
    };
    write_row(tx, &row)?;
    Ok(row)
}

/// The settlement of a fresh request: the rework gates first, then the split
/// of the request against the run's other open slices.
fn decide(
    conn: &Connection,
    task: &str,
    run: u32,
    request: &BlockedScope,
    requests: u32,
) -> Result<Settled, VivariumError> {
    if requests >= 2 {
        return Ok(rework(format!("second blocked-scope request on {task}")));
    }
    if request.paths.len() > MAX_REQUEST_PATHS {
        return Ok(rework(format!(
            "{} paths requested; the limit is {MAX_REQUEST_PATHS}",
            request.paths.len()
        )));
    }
    if request.design {
        return Ok(rework(format!("design question: {}", request.reason)));
    }
    let split = split_request(&request.paths, &open_scopes(conn, task, run)?);
    let mut record = split.holders.clone();
    record.extend(
        split
            .held_paths
            .iter()
            .map(|path| format!("{HELD_PATH_PREFIX}{path}")),
    );
    let state = if split.holders.is_empty() {
        ScopeState::Clear
    } else {
        ScopeState::Wait
    };
    let resolution = if split.holders.is_empty() {
        Resolution::Widen(split.free.clone())
    } else {
        Resolution::WaitOn(split.holders.clone())
    };
    Ok(Settled {
        resolution,
        free: split.free,
        holders: split.holders,
        record,
        state,
    })
}

/// A settlement that sends the slice to the slicer instead.
fn rework(reason: String) -> Settled {
    Settled {
        resolution: Resolution::Rework(reason),
        free: Vec::new(),
        holders: Vec::new(),
        record: Vec::new(),
        state: ScopeState::Rework,
    }
}

/// A request split by who holds its paths.
struct Split {
    /// The open slices whose scope shares a repository path with the request.
    holders: Vec<String>,
    /// Requested paths no open slice holds.
    free: Vec<String>,
    /// Requested paths some open slice holds.
    held_paths: Vec<String>,
}

fn split_request(paths: &[String], others: &[(String, Vec<String>)]) -> Split {
    let mut split = Split {
        holders: Vec::new(),
        free: Vec::new(),
        held_paths: Vec::new(),
    };
    for path in paths {
        let mut held = false;
        for (other, scope) in others {
            if scope.iter().any(|entry| overlaps(entry, path)) {
                held = true;
                if !split.holders.contains(other) {
                    split.holders.push(other.clone());
                }
            }
        }
        if held {
            split.held_paths.push(path.clone());
        } else {
            split.free.push(path.clone());
        }
    }
    split
}

/// True when a scope entry and a requested path share at least one repository
/// path: the same path, a holder directory containing the requested file, a
/// requested directory enclosing a held file, or two directories with one
/// inside the other. Both sides normalize through [`ScopePath`], so leading
/// `./` and root spellings compare by component, never by string prefix
/// (`src/` does not touch `src-old/`, and a file entry named `src/pump` does
/// not cover `src/pump/run.rs`). A declaration that fails normalization can
/// only match an identical one.
fn overlaps(entry: &str, path: &str) -> bool {
    match (ScopePath::parse(entry), ScopePath::parse(path)) {
        (Ok(entry), Ok(path)) => entry.intersects(&path),
        _ => entry == path,
    }
}

/// Count one more blocked-scope request on the task; returns the new count.
fn bump_requests(conn: &Connection, task: &str) -> Result<u32, VivariumError> {
    let changed = conn
        .execute(
            "UPDATE slice_scope SET scope_requests = scope_requests + 1 WHERE task = ?1",
            params![task],
        )
        .map_err(db_err("bump scope requests"))?;
    if changed == 0 {
        return Err(no_scope(task));
    }
    conn.query_row(
        "SELECT scope_requests FROM slice_scope WHERE task = ?1",
        params![task],
        |row| row.get::<_, i64>(0),
    )
    .map(from_u32)
    .map_err(db_err("read scope requests"))
}

/// Add `paths` to a slice's write scope; paths already present are kept once.
fn widen_scope(conn: &Connection, task: &str, paths: &[String]) -> Result<(), VivariumError> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT write_scope_json FROM slice_scope WHERE task = ?1",
            params![task],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err("read slice scope"))?;
    let mut scope: Vec<String> = stored
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(db_err("decode slice scope"))?
        .ok_or_else(|| no_scope(task))?;
    for path in paths {
        if !scope.contains(path) {
            scope.push(path.clone());
        }
    }
    update_scope(conn, "write_scope_json", task, &json(&scope)?)
}

/// Set one `slice_scope` column of `task`.
fn update_scope(
    conn: &Connection,
    column: &str,
    task: &str,
    value: &str,
) -> Result<(), VivariumError> {
    let changed = conn
        .execute(
            &format!("UPDATE slice_scope SET {column} = ?2 WHERE task = ?1"),
            params![task, value],
        )
        .map_err(db_err("update slice scope"))?;
    if changed == 0 {
        return Err(no_scope(task));
    }
    Ok(())
}

/// The write scopes of the other open slices of `task`'s run, as
/// `(task, write scope)` pairs. The query mirrors `flow_index`'s
/// `open_scopes_of_run`: the settlement reads it inside its transaction.
fn open_scopes(
    conn: &Connection,
    task: &str,
    run: u32,
) -> Result<Vec<(String, Vec<String>)>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT s.task, s.write_scope_json FROM slice_scope s \
             JOIN flow_steps f ON f.task = s.task \
             WHERE s.task != ?1 AND s.run = ?2 AND f.run = ?2 \
               AND f.need = (SELECT need FROM flow_steps WHERE task = ?1) \
               AND f.state NOT IN ('done', 'failed') \
             ORDER BY f.rowid",
        )
        .map_err(db_err("read open slice scopes"))?;
    let found = stmt
        .query_map(params![task, run], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(db_err("read open slice scopes"))?;
    found
        .map(|raw| {
            raw.map_err(db_err("read open slice scopes"))
                .and_then(|(task, text)| {
                    Ok((
                        task,
                        serde_json::from_str(&text).map_err(db_err("decode list"))?,
                    ))
                })
        })
        .collect()
}

fn no_scope(task: &str) -> VivariumError {
    VivariumError::Other(format!("pump.db slice_scope: no scope for task {task}"))
}

/// The stored form of a resolution with its three encoded fields still raw:
/// request, resolution, holders, state word.
type RawResolution = (ScopeResolutionRow, String, String, String, String);

fn raw_row(row: &Row<'_>) -> rusqlite::Result<RawResolution> {
    Ok((
        ScopeResolutionRow {
            source_job: row.get(0)?,
            task: row.get(1)?,
            run: from_u32(row.get(2)?),
            request: BlockedScope {
                paths: Vec::new(),
                reason: String::new(),
                design: false,
            },
            resolution: Resolution::Widen(Vec::new()),
            requests: from_u32(row.get(5)?),
            holders: Vec::new(),
            state: ScopeState::Clear,
            created_at: row.get(8)?,
        },
        row.get(3)?,
        row.get(4)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn finish_row(tuple: RawResolution) -> Result<ScopeResolutionRow, VivariumError> {
    let (mut row, request, resolution, holders, state) = tuple;
    row.request =
        serde_json::from_str(&request).map_err(db_err("decode scope resolution request"))?;
    row.resolution =
        serde_json::from_str(&resolution).map_err(db_err("decode scope resolution outcome"))?;
    row.holders =
        serde_json::from_str(&holders).map_err(db_err("decode scope resolution holders"))?;
    row.state = ScopeState::parse(&state)?;
    Ok(row)
}

fn rows(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<ScopeResolutionRow>, VivariumError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(db_err("read scope resolutions"))?;
    let found = stmt
        .query_map(args, raw_row)
        .map_err(db_err("read scope resolutions"))?;
    found
        .map(|raw| {
            raw.map_err(db_err("read scope resolutions"))
                .and_then(finish_row)
        })
        .collect()
}

fn read_by_source(
    conn: &Connection,
    source_job: &str,
) -> Result<Option<ScopeResolutionRow>, VivariumError> {
    Ok(rows(
        conn,
        &format!("SELECT {COLUMNS} FROM scope_resolutions WHERE source_job = ?1"),
        [source_job],
    )?
    .pop())
}

fn write_row(conn: &Connection, row: &ScopeResolutionRow) -> Result<(), VivariumError> {
    conn.execute(
        "INSERT INTO scope_resolutions (
             source_job, task, run, request_json, resolution_json,
             requests, holders_json, state, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            row.source_job,
            row.task,
            i64::from(row.run),
            json(&row.request)?,
            json(&row.resolution)?,
            i64::from(row.requests),
            json(&row.holders)?,
            row.state.word(),
            row.created_at,
        ],
    )
    .map_err(db_err("insert scope resolution"))?;
    Ok(())
}

fn now_unix() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(0)
}

#[cfg(test)]
#[path = "scope_resolution_test.rs"]
mod tests;
