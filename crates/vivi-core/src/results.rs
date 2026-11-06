//! The project's managed results database (`results.db`).
//!
//! Repeated checks hand in one row per case. The database lives beside the
//! pump's own `pump.db`, in `<project>/.vivi/pump/results.db`, so a project's
//! execution evidence stays with the rest of its managed state. Runs carry the
//! job and task they came from; rows are unique by case within a run.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use vivi_error::VivariumError;

/// The database filename inside the pump home.
pub const DB_FILENAME: &str = "results.db";

/// The schema version written to `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 1;

/// The statuses a submitted case may carry.
pub const STATUSES: [&str; 7] = [
    "pass",
    "fail",
    "error",
    "skip",
    "rejected",
    "expected-fail",
    "not-applicable",
];

/// Longest accepted case id, in characters.
pub const MAX_CASE_CHARS: usize = 512;

/// Longest accepted phase, in characters.
pub const MAX_PHASE_CHARS: usize = 128;

/// Longest stored diagnostic (its first line), in characters.
pub const MAX_DIAGNOSTIC_CHARS: usize = 512;

const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY,
  project TEXT NOT NULL,
  role TEXT NOT NULL,
  target TEXT NOT NULL,
  suite TEXT NOT NULL,
  commit_sha TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  host TEXT NOT NULL,
  job TEXT,
  task TEXT
);
CREATE INDEX IF NOT EXISTS runs_by_commit_idx
  ON runs(project, target, suite, commit_sha);
CREATE TABLE IF NOT EXISTS rows (
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  case_name TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('pass', 'fail', 'error', 'skip',
    'rejected', 'expected-fail', 'not-applicable')),
  phase TEXT,
  diagnostic TEXT,
  millis INTEGER,
  PRIMARY KEY (run_id, case_name)
);
CREATE INDEX IF NOT EXISTS rows_by_run_idx ON rows(run_id);
";

/// Where the results database for a project root lives.
#[must_use]
pub fn db_path(project_root: &Path) -> PathBuf {
    project_root.join(".vivi").join("pump").join(DB_FILENAME)
}

/// One run (one suite of one check execution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub project: String,
    pub role: String,
    pub target: String,
    pub suite: String,
    pub commit: String,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub host: String,
    pub job: Option<String>,
    pub task: Option<String>,
}

/// One case result of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultRow {
    /// The case's id in its suite.
    pub case: String,
    /// One of [`STATUSES`].
    pub status: String,
    /// Optional phase or shard name.
    #[serde(default)]
    pub phase: Option<String>,
    /// Optional diagnostic; only its first line is stored.
    #[serde(default)]
    pub diagnostic: Option<String>,
    /// Optional duration in milliseconds.
    #[serde(default)]
    pub millis: Option<u64>,
}

/// A stored row, with the run it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRow {
    pub run_id: String,
    pub case: String,
    pub status: String,
    pub phase: Option<String>,
    pub diagnostic: Option<String>,
    pub millis: Option<u64>,
}

/// Validate a submitted row, returning its stored form (the first diagnostic
/// line, trimmed optional fields).
///
/// # Errors
/// Returns the reason a field is empty, unknown, or over its bound.
pub fn validate_row(row: ResultRow) -> Result<ResultRow, String> {
    let case = row.case.trim().to_owned();
    if case.is_empty() {
        return Err(String::from("case is empty"));
    }
    if case.chars().count() > MAX_CASE_CHARS {
        return Err(format!("case is longer than {MAX_CASE_CHARS} characters"));
    }
    if !STATUSES.contains(&row.status.as_str()) {
        return Err(format!("unknown status `{}`", row.status));
    }
    let phase = row
        .phase
        .map(|phase| phase.trim().to_owned())
        .filter(|phase| !phase.is_empty());
    if let Some(phase) = &phase
        && phase.chars().count() > MAX_PHASE_CHARS
    {
        return Err(format!("phase is longer than {MAX_PHASE_CHARS} characters"));
    }
    let diagnostic = row
        .diagnostic
        .as_deref()
        .and_then(|diagnostic| diagnostic.lines().next())
        .map(str::trim)
        .filter(|diagnostic| !diagnostic.is_empty());
    if let Some(diagnostic) = diagnostic
        && diagnostic.chars().count() > MAX_DIAGNOSTIC_CHARS
    {
        return Err(format!(
            "diagnostic is longer than {MAX_DIAGNOSTIC_CHARS} characters"
        ));
    }
    Ok(ResultRow {
        case,
        status: row.status,
        phase,
        diagnostic: diagnostic.map(str::to_owned),
        millis: row.millis,
    })
}

/// A handle to a project's results database.
pub struct ResultsStore {
    conn: Connection,
}

impl ResultsStore {
    /// Open or create `path`, ensure its schema, and refuse a newer one.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the directory or file cannot be opened,
    /// the schema cannot be read or written, or the file is newer than this
    /// binary.
    pub fn open(path: &Path) -> Result<Self, VivariumError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(db_error)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_error)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_error)?;
        let version = schema_version(&conn)?;
        if version > SCHEMA_VERSION {
            return Err(VivariumError::Message(format!(
                "results.db schema {version} is newer than this binary ({SCHEMA_VERSION}); refusing to open"
            )));
        }
        if version < SCHEMA_VERSION {
            migrate(&conn, version)?;
        }
        Ok(Self { conn })
    }

    /// Open the results database belonging to `project_root`.
    ///
    /// # Errors
    /// As [`ResultsStore::open`].
    pub fn open_project(project_root: &Path) -> Result<Self, VivariumError> {
        Self::open(&db_path(project_root))
    }

    /// The schema version this database carries.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when `PRAGMA user_version` cannot be read.
    pub fn schema_version(&self) -> Result<i64, VivariumError> {
        schema_version(&self.conn)
    }

    /// Open a run. Its id must be new.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the insert fails (including a duplicate
    /// id).
    pub fn open_run(&mut self, run: &Run) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT INTO runs (id, project, role, target, suite, commit_sha,
                started_at, finished_at, host, job, task)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    run.id,
                    run.project,
                    run.role,
                    run.target,
                    run.suite,
                    run.commit,
                    run.started_at,
                    run.finished_at,
                    run.host,
                    run.job,
                    run.task,
                ],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// Close a run, stamping `finished_at`. `false` when the run is unknown;
    /// a run that was already closed keeps its first stamp.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the update fails.
    pub fn close_run(&mut self, id: &str, at: u64) -> Result<bool, VivariumError> {
        let changed = self
            .conn
            .execute(
                "UPDATE runs SET finished_at = ?2 WHERE id = ?1 AND finished_at IS NULL",
                params![id, at],
            )
            .map_err(db_error)?;
        if changed > 0 {
            return Ok(true);
        }
        Ok(self.run(id)?.is_some())
    }

    /// The run with `id`, when it exists.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the query fails.
    pub fn run(&self, id: &str) -> Result<Option<Run>, VivariumError> {
        let run = self
            .conn
            .query_row(
                "SELECT id, project, role, target, suite, commit_sha, started_at,
                        finished_at, host, job, task
                 FROM runs WHERE id = ?1",
                [id],
                row_from_sql,
            )
            .optional()
            .map_err(db_error)?;
        Ok(run)
    }

    /// Insert validated rows into an open run, all in one transaction.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the run is unknown or closed, a case
    /// repeats within the batch or the run, or the insert fails.
    pub fn insert_rows(
        &mut self,
        run_id: &str,
        rows: &[ResultRow],
    ) -> Result<usize, VivariumError> {
        let run = self
            .run(run_id)?
            .ok_or_else(|| VivariumError::Message(format!("unknown run `{run_id}`")))?;
        if run.finished_at.is_some() {
            return Err(VivariumError::Message(format!("run `{run_id}` is closed")));
        }
        let rows = rows
            .iter()
            .cloned()
            .map(validate_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(VivariumError::Message)?;
        let mut seen = BTreeSet::new();
        for row in &rows {
            if !seen.insert(row.case.clone()) {
                return Err(VivariumError::Message(format!(
                    "case `{}` was submitted twice in one run",
                    row.case
                )));
            }
            let exists: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM rows WHERE run_id = ?1 AND case_name = ?2",
                    params![run_id, row.case],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if exists > 0 {
                return Err(VivariumError::Message(format!(
                    "case `{}` was submitted twice in one run",
                    row.case
                )));
            }
        }
        let tx = self.conn.transaction().map_err(db_error)?;
        for row in &rows {
            tx.execute(
                "INSERT INTO rows (run_id, case_name, status, phase, diagnostic, millis)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    run_id,
                    row.case,
                    row.status,
                    row.phase,
                    row.diagnostic,
                    row.millis,
                ],
            )
            .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok(rows.len())
    }

    /// Runs of `(project, target, suite)` for exactly `commit`, newest first.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the query fails.
    pub fn runs_for_commit(
        &self,
        project: &str,
        target: &str,
        suite: &str,
        commit: &str,
    ) -> Result<Vec<Run>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, project, role, target, suite, commit_sha, started_at,
                    finished_at, host, job, task
             FROM runs
             WHERE project = ?1 AND target = ?2 AND suite = ?3 AND commit_sha = ?4
             ORDER BY started_at DESC, id",
            )
            .map_err(db_error)?;
        let runs = stmt
            .query_map(params![project, target, suite, commit], row_from_sql)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(runs)
    }

    /// The rows of `run_id`, ordered by case.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the query fails.
    pub fn rows(&self, run_id: &str) -> Result<Vec<StoredRow>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT run_id, case_name, status, phase, diagnostic, millis
             FROM rows WHERE run_id = ?1 ORDER BY case_name",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map([run_id], |row| {
                Ok(StoredRow {
                    run_id: row.get(0)?,
                    case: row.get(1)?,
                    status: row.get(2)?,
                    phase: row.get(3)?,
                    diagnostic: row.get(4)?,
                    millis: row.get(5)?,
                })
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        Ok(rows)
    }
}

fn db_error(error: rusqlite::Error) -> VivariumError {
    VivariumError::Other(format!("results.db: {error}"))
}

fn schema_version(conn: &Connection) -> Result<i64, VivariumError> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| VivariumError::Other(format!("results.db read user_version: {error}")))
}

fn migrate(conn: &Connection, from: i64) -> Result<(), VivariumError> {
    if from < 1 {
        conn.execute_batch(SCHEMA_DDL).map_err(db_error)?;
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(db_error)?;
    Ok(())
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: row.get(0)?,
        project: row.get(1)?,
        role: row.get(2)?,
        target: row.get(3)?,
        suite: row.get(4)?,
        commit: row.get(5)?,
        started_at: row.get(6)?,
        finished_at: row.get(7)?,
        host: row.get(8)?,
        job: row.get(9)?,
        task: row.get(10)?,
    })
}

#[cfg(test)]
#[path = "results_test.rs"]
mod tests;
