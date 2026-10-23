//! The pump's SQLite store (`pump.db`).
//!
//! `pump.db` is the pump's live memory. The in-memory [`State`] remains the
//! decision model: one [`PumpStore::load`] at startup, one
//! [`PumpStore::save`] per save point, each in a single transaction. The
//! `state.json` shape is kept as the import/export format so a predecessor
//! pump can still read a converted file.

use std::fs::{self, File};
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::error::VivariumError;

mod codec;
mod flow_index;
pub mod flow_links;
mod main_state;
mod need_branch;
mod priority;
mod schema;
mod snapshot;
mod state_rows;
pub mod step_payload;
mod unit_extra;
pub(super) use codec::{db_err, from_json, from_u32, from_u64, json, json_or_null};
pub use need_branch::NeedBranchRow;
use schema::{drop_request_queues, newer_schema, schema_version, take_migration_lock};
use state_rows::{
    clear, load_meta, load_needs, load_order, load_strings, load_units, write_lists, write_meta,
    write_needs, write_units,
};

use super::state::State;

/// The schema version written to `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 8;

/// The database filename inside the pump home.
pub const DB_FILENAME: &str = "pump.db";

const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS needs (
  handle TEXT PRIMARY KEY,
  verdict TEXT NOT NULL,
  reason TEXT NOT NULL,
  units_json TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 0,
  thread_sha256 TEXT,
  thread_seen_at INTEGER,
  amendment_pending INTEGER NOT NULL DEFAULT 0,
  withdrawn INTEGER NOT NULL DEFAULT 0,
  extra_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS plan_revisions (
  need TEXT NOT NULL,
  revision INTEGER NOT NULL,
  verdict TEXT NOT NULL,
  reason TEXT NOT NULL,
  body_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (need, revision)
);
CREATE TABLE IF NOT EXISTS units (
  id TEXT PRIMARY KEY,
  need TEXT NOT NULL,
  target TEXT NOT NULL DEFAULT 'default',
  revision INTEGER NOT NULL DEFAULT 1,
  title TEXT NOT NULL,
  brief TEXT NOT NULL,
  write_scope_json TEXT NOT NULL,
  after_json TEXT NOT NULL,
  proof_json TEXT NOT NULL,
  tools TEXT NOT NULL,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL,
  task TEXT,
  branch TEXT,
  model TEXT,
  since INTEGER NOT NULL,
  since_dispatch INTEGER,
  pool TEXT,
  commit_sha TEXT,
  merged_at INTEGER,
  outside_json TEXT NOT NULL,
  replaced_by_json TEXT NOT NULL,
  verdict_text TEXT,
  kind_reported INTEGER NOT NULL,
  extra_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS units_need_idx ON units(need);
CREATE INDEX IF NOT EXISTS units_status_idx ON units(status);
CREATE TABLE IF NOT EXISTS unit_edges (
  unit TEXT NOT NULL,
  after_unit TEXT NOT NULL,
  PRIMARY KEY (unit, after_unit)
);
CREATE TABLE IF NOT EXISTS unit_events (
  event_id INTEGER PRIMARY KEY AUTOINCREMENT,
  unit TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  model TEXT,
  why TEXT NOT NULL,
  detail TEXT NOT NULL,
  at INTEGER NOT NULL,
  branch TEXT,
  task TEXT
);
CREATE INDEX IF NOT EXISTS unit_events_unit_idx ON unit_events(unit, event_id);
CREATE TABLE IF NOT EXISTS unit_order (
  position INTEGER PRIMARY KEY,
  unit TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS merges (
  unit TEXT PRIMARY KEY,
  commit_sha TEXT NOT NULL,
  at INTEGER NOT NULL,
  outside_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS gates (
  gate_id INTEGER PRIMARY KEY AUTOINCREMENT,
  tip TEXT NOT NULL,
  at INTEGER NOT NULL,
  merges INTEGER NOT NULL,
  names_json TEXT,
  new_json TEXT
);
CREATE TABLE IF NOT EXISTS gate_failures (
  gate_id INTEGER NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  PRIMARY KEY (gate_id, name, kind)
);
CREATE TABLE IF NOT EXISTS exceptions (
  event_id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  kind TEXT NOT NULL,
  unit TEXT NOT NULL,
  text TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS closed_needs (need TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS plans_seen (name TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS promotions (
  tip TEXT PRIMARY KEY,
  at INTEGER NOT NULL,
  failing_json TEXT
);
CREATE TABLE IF NOT EXISTS red (name TEXT PRIMARY KEY, unit TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS red_reported (value TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS pump_control (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  mode TEXT NOT NULL CHECK (mode IN ('running', 'paused', 'stopped')),
  shutdown_requested INTEGER NOT NULL DEFAULT 0 CHECK (shutdown_requested IN (0, 1))
);
CREATE TABLE IF NOT EXISTS need_priority (
  need TEXT PRIMARY KEY,
  level INTEGER NOT NULL,
  set_at INTEGER NOT NULL
);
";

/// A connection to `pump.db`.
pub struct PumpStore {
    conn: Connection,
    // Keep the project pump lock for the full lifetime of a schema migration.
    _migration_lock: Option<File>,
}

impl PumpStore {
    /// Open or create `path`, set the pump's SQLite pragmas, and ensure the
    /// schema.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the directory or file cannot be
    /// created, the connection cannot be configured, or the schema fails.
    pub fn open(path: &Path) -> Result<Self, VivariumError> {
        Self::open_with_lock_policy(path, false)
    }

    /// Open the store while the caller already owns the project's pump lock.
    /// Pump startup uses this path to migrate an older schema after excluding
    /// every other pump and migration process.
    pub(crate) fn open_for_run(path: &Path) -> Result<Self, VivariumError> {
        Self::open_with_lock_policy(path, true)
    }

    fn open_with_lock_policy(
        path: &Path,
        caller_holds_pump_lock: bool,
    ) -> Result<Self, VivariumError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path).map_err(db_err("open"))?;
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(db_err("set busy_timeout"))?;
        let version = schema_version(&conn)?;
        if version > SCHEMA_VERSION {
            return Err(newer_schema(version));
        }
        let migration_lock = if version < SCHEMA_VERSION && !caller_holds_pump_lock {
            Some(take_migration_lock(path)?)
        } else {
            None
        };
        ensure_schema(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err("set journal_mode"))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(db_err("set synchronous"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            conn,
            _migration_lock: migration_lock,
        })
    }

    /// Open `path` read-only. A missing file yields `None`, so a reader can
    /// render "not local" without creating an empty database.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the file exists but cannot be opened.
    pub fn open_read_only(path: &Path) -> Result<Option<Self>, VivariumError> {
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(db_err("open read-only"))?;
        conn.busy_timeout(Duration::from_secs(1))
            .map_err(db_err("set busy_timeout"))?;
        conn.pragma_update(None, "query_only", true)
            .map_err(db_err("set query_only"))?;
        Ok(Some(Self {
            conn,
            _migration_lock: None,
        }))
    }

    /// The `PRAGMA user_version` of an open store.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the pragma cannot be read.
    pub fn schema_version(&self) -> Result<i64, VivariumError> {
        self.conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_err("read user_version"))
    }

    /// True when no state has been written yet. Used to decide whether a
    /// `state.json` beside the database should be imported.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the marker cannot be read.
    pub fn is_empty(&self) -> Result<bool, VivariumError> {
        let marker: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'initialized'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read initialized marker"))?;
        Ok(marker.as_deref() != Some("1"))
    }

    /// Read the persisted operator mode, or `None` before first initialization.
    pub fn control_mode(&self) -> Result<Option<String>, VivariumError> {
        self.conn
            .query_row(
                "SELECT mode FROM pump_control WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read pump control mode"))
    }

    /// Initialize control state once, preserving the migrated legacy mode.
    pub fn initialize_control(&mut self, legacy_mode: &str) -> Result<String, VivariumError> {
        let tx = self
            .conn
            .transaction()
            .map_err(db_err("begin control init"))?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT mode FROM pump_control WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read pump control mode"))?;
        let mode = if let Some(mode) = existing {
            mode
        } else {
            tx.execute(
                "INSERT INTO pump_control (singleton, mode) VALUES (1, ?1)",
                [legacy_mode],
            )
            .map_err(db_err("initialize pump control mode"))?;
            legacy_mode.to_string()
        };
        tx.commit().map_err(db_err("commit control init"))?;
        Ok(mode)
    }

    /// Change the durable operator mode without altering transient shutdown state.
    pub fn set_control_mode(&mut self, mode: &str) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE pump_control SET mode = ?1 WHERE singleton = 1",
                [mode],
            )
            .map_err(db_err("write pump control mode"))?;
        Ok(())
    }

    /// Whether the service requested this pump process to exit after its tick.
    pub fn shutdown_requested(&self) -> Result<bool, VivariumError> {
        self.conn
            .query_row(
                "SELECT shutdown_requested FROM pump_control WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|value| value.unwrap_or(0) != 0)
            .map_err(db_err("read pump shutdown request"))
    }

    /// Set transient service shutdown state while keeping the operator mode.
    pub fn set_shutdown_requested(&mut self, requested: bool) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "UPDATE pump_control SET shutdown_requested = ?1 WHERE singleton = 1",
                [i64::from(requested)],
            )
            .map_err(db_err("write pump shutdown request"))?;
        Ok(())
    }

    /// Read the whole state.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when any table cannot be read or a stored
    /// value cannot be decoded.
    pub fn load(&self) -> Result<State, VivariumError> {
        let mut st = State {
            needs: load_needs(&self.conn)?,
            units: load_units(&self.conn)?,
            order: load_order(&self.conn)?,
            closed_needs: load_strings(&self.conn, "closed_needs", "need")?,
            plans_seen: load_strings(&self.conn, "plans_seen", "name")?,
            ..State::default()
        };
        load_meta(&self.conn, &mut st)?;
        crate::pump::need::backfill_legacy_unit_revisions(&mut st);
        Ok(st)
    }

    /// Write the whole state in one transaction.
    ///
    /// The first cut replaces every row. At the live pump's size (a few
    /// hundred units) this is cheaper than rewriting the 3.6 MB JSON file,
    /// and it keeps save a single atomic step. Dirty tracking is a later
    /// optimization.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the transaction or any write fails.
    pub fn save(&mut self, st: &State) -> Result<(), VivariumError> {
        let tx = self.conn.transaction().map_err(db_err("begin"))?;
        clear(&tx)?;
        write_meta(&tx, st)?;
        write_units(&tx, st)?;
        write_needs(&tx, st)?;
        write_lists(&tx, st)?;
        tx.commit().map_err(db_err("commit"))?;
        Ok(())
    }

    /// Write the whole state as the `state.json` shape at `path`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when loading or writing fails.
    pub fn export_json(&self, path: &Path) -> Result<(), VivariumError> {
        self.load()?.save(path)
    }

    /// Record one accepted plan revision.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the insert fails.
    pub fn record_plan_revision(
        &mut self,
        need: &str,
        revision: u64,
        verdict: &str,
        reason: &str,
        body: &str,
        at: u64,
    ) -> Result<(), VivariumError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO plan_revisions
                   (need, revision, verdict, reason, body_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    need,
                    i64::try_from(revision).unwrap_or(0),
                    verdict,
                    reason,
                    body,
                    i64::try_from(at).unwrap_or(0),
                ],
            )
            .map_err(db_err("write plan revision"))?;
        Ok(())
    }

    /// Read the exception rows, oldest first: `(at, kind, unit, text)`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub fn exceptions(&self) -> Result<Vec<(u64, String, String, String)>, VivariumError> {
        let mut stmt = self
            .conn
            .prepare("SELECT at, kind, unit, text FROM exceptions ORDER BY event_id")
            .map_err(db_err("read exceptions"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((from_u64(row.get(0)?), row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(db_err("read exceptions"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(db_err("read exceptions"))
    }
}

/// Record one accepted plan revision in `<home>/pump.db`.
///
/// # Errors
/// Returns [`VivariumError`] when the database cannot be opened or written.
pub fn record_plan_revision(
    home: &Path,
    need: &str,
    revision: u64,
    verdict: &str,
    reason: &str,
    body: &str,
    at: u64,
) -> Result<(), VivariumError> {
    let mut store = PumpStore::open(&home.join(DB_FILENAME))?;
    store.record_plan_revision(need, revision, verdict, reason, body, at)
}

/// Append one exception row to `<home>/pump.db`. An exception is a report to
/// a person, so a database failure is ignored: the caller has already
/// appended the same text to `exceptions.txt`.
pub fn append_exception(home: &Path, at: u64, kind: &str, unit: &str, text: &str) {
    let path = home.join(DB_FILENAME);
    let Ok(store) = PumpStore::open(&path) else {
        return;
    };
    let at = i64::try_from(at).unwrap_or(0);
    drop(store.conn.execute(
        "INSERT INTO exceptions (at, kind, unit, text) VALUES (?1, ?2, ?3, ?4)",
        params![at, kind, unit, text],
    ));
}

/// Ensure the schema and write the schema version.
fn ensure_schema(conn: &Connection) -> Result<(), VivariumError> {
    let version = schema_version(conn)?;
    if version > SCHEMA_VERSION {
        return Err(newer_schema(version));
    }
    conn.execute_batch(SCHEMA_DDL)
        .map_err(db_err("create schema"))?;
    drop_request_queues(conn)?;
    flow_index::ensure_tables(conn)?;
    flow_links::ensure_tables(conn)?;
    step_payload::ensure_table(conn)?;
    if version > 0 && version < SCHEMA_VERSION {
        migrate_older_schema(conn)?;
    }
    if version != SCHEMA_VERSION {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(db_err("write user_version"))?;
    }
    Ok(())
}

/// Add columns introduced after schema version 1. A fresh schema already has
/// them, so duplicate-column errors are expected and ignored.
fn migrate_older_schema(conn: &Connection) -> Result<(), VivariumError> {
    for (table, column, ddl) in [
        ("needs", "revision", "INTEGER NOT NULL DEFAULT 0"),
        ("needs", "thread_sha256", "TEXT"),
        ("needs", "thread_seen_at", "INTEGER"),
        ("needs", "amendment_pending", "INTEGER NOT NULL DEFAULT 0"),
        ("needs", "withdrawn", "INTEGER NOT NULL DEFAULT 0"),
        ("units", "revision", "INTEGER NOT NULL DEFAULT 1"),
        ("units", "target", "TEXT NOT NULL DEFAULT 'default'"),
        ("unit_events", "branch", "TEXT"),
        ("unit_events", "task", "TEXT"),
    ] {
        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}");
        if conn.execute(&sql, []).is_err() {
            // The column already exists (fresh schema).
        }
    }
    Ok(())
}
