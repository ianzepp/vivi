//! The pump's SQLite store (`pump.db`).
//!
//! `pump.db` is the pump's live memory. The in-memory [`State`] remains the
//! decision model: one [`PumpStore::load`] at startup, one
//! [`PumpStore::save`] per save point, each in a single transaction. The
//! `state.json` shape is kept as the import/export format so a predecessor
//! pump can still read a converted file.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use parking_lot::{ReentrantMutex, ReentrantMutexGuard};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use vivi_error::VivariumError;

pub mod build_waits;
mod codec;
mod flow_index;
pub mod flow_links;
mod job_release;
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

/// A handle to this process's one connection to a `pump.db`.
///
/// Every handle for the same file in one process shares a single connection,
/// opened on first use and closed when the last handle drops. The pump holds
/// its startup handle for the whole run, so a tick's reads and saves reuse that
/// connection instead of paying for an open, a schema check and a WAL
/// checkpoint each time. The lock is reentrant: code holding a handle can still
/// record an exception through another handle on the same thread.
pub struct PumpStore {
    shared: Arc<Shared>,
}

struct Shared {
    conn: ReentrantMutex<Connection>,
    /// The state as this connection last loaded or saved it, serialized. A save
    /// of an identical state is skipped: the tables already hold it.
    last_state: Mutex<Option<Vec<u8>>>,
    // Keep the project pump lock for the full lifetime of a schema migration.
    _migration_lock: Option<File>,
}

/// The live connection for each open `pump.db` path in this process.
static OPEN: Mutex<BTreeMap<PathBuf, Weak<Shared>>> = Mutex::new(BTreeMap::new());

/// The live shared connection for `path`, if a handle still holds one.
fn live(path: &Path) -> Option<Arc<Shared>> {
    let open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    open.get(path).and_then(Weak::upgrade)
}

/// Record `shared` as the live connection for `path`, dropping dead entries.
fn register(path: &Path, shared: &Arc<Shared>) {
    let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    open.retain(|_, weak| weak.strong_count() > 0);
    open.insert(path.to_path_buf(), Arc::downgrade(shared));
}

impl PumpStore {
    /// The shared connection, locked for the caller's use.
    fn conn(&self) -> ReentrantMutexGuard<'_, Connection> {
        self.shared.conn.lock()
    }

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
    pub fn open_for_run(path: &Path) -> Result<Self, VivariumError> {
        Self::open_with_lock_policy(path, true)
    }

    fn open_with_lock_policy(
        path: &Path,
        caller_holds_pump_lock: bool,
    ) -> Result<Self, VivariumError> {
        if let Some(shared) = live(path) {
            return Ok(Self { shared });
        }
        let shared = Arc::new(Self::connect(path, caller_holds_pump_lock)?);
        register(path, &shared);
        Ok(Self { shared })
    }

    /// Open a new connection to `path`: pragmas, schema, and file mode.
    fn connect(path: &Path, caller_holds_pump_lock: bool) -> Result<Shared, VivariumError> {
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
        Ok(Shared {
            conn: ReentrantMutex::new(conn),
            last_state: Mutex::new(None),
            _migration_lock: migration_lock,
        })
    }

    /// Open `path` read-only. A missing file yields `None`, so a reader can
    /// render "not local" without creating an empty database. A process that
    /// already holds the read-write connection (the pump) reads through it.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the file exists but cannot be opened.
    pub fn open_read_only(path: &Path) -> Result<Option<Self>, VivariumError> {
        if let Some(shared) = live(path) {
            return Ok(Some(Self { shared }));
        }
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
            shared: Arc::new(Shared {
                conn: ReentrantMutex::new(conn),
                last_state: Mutex::new(None),
                _migration_lock: None,
            }),
        }))
    }

    /// The `PRAGMA user_version` of an open store.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the pragma cannot be read.
    pub fn schema_version(&self) -> Result<i64, VivariumError> {
        self.conn()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_err("read user_version"))
    }

    /// True when no state has been written yet. Used to decide whether a
    /// `state.json` beside the database should be imported.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the marker cannot be read.
    pub fn is_empty(&self) -> Result<bool, VivariumError> {
        let conn = self.conn();
        let marker: Option<String> = conn
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
        self.conn()
            .query_row(
                "SELECT mode FROM pump_control WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err("read pump control mode"))
    }

    /// Initialize control state once, preserving the migrated legacy mode.
    pub fn initialize_control(&self, legacy_mode: &str) -> Result<String, VivariumError> {
        let conn = self.conn();
        let tx = conn
            .unchecked_transaction()
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
    pub fn set_control_mode(&self, mode: &str) -> Result<(), VivariumError> {
        self.conn()
            .execute(
                "UPDATE pump_control SET mode = ?1 WHERE singleton = 1",
                [mode],
            )
            .map_err(db_err("write pump control mode"))?;
        Ok(())
    }

    /// Whether the service requested this pump process to exit after its tick.
    pub fn shutdown_requested(&self) -> Result<bool, VivariumError> {
        self.conn()
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
    pub fn set_shutdown_requested(&self, requested: bool) -> Result<(), VivariumError> {
        self.conn()
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
            needs: load_needs(&self.conn())?,
            units: load_units(&self.conn())?,
            order: load_order(&self.conn())?,
            closed_needs: load_strings(&self.conn(), "closed_needs", "need")?,
            plans_seen: load_strings(&self.conn(), "plans_seen", "name")?,
            ..State::default()
        };
        load_meta(&self.conn(), &mut st)?;
        self.remember(snapshot(&st));
        Ok(st)
    }

    /// Record `snapshot` as the state the tables now hold.
    fn remember(&self, snapshot: Option<Vec<u8>>) {
        *self
            .shared
            .last_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = snapshot;
    }

    /// True when `snapshot` is the state the tables already hold.
    fn holds(&self, snapshot: Option<&Vec<u8>>) -> bool {
        let last = self
            .shared
            .last_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        snapshot.is_some() && last.as_ref() == snapshot
    }

    /// Write the whole state in one transaction.
    ///
    /// A save replaces every row, in one atomic step. A state identical to the
    /// one this connection last loaded or saved is not written again: the run
    /// loop saves several times a tick, and an idle tick changes nothing. A
    /// skipped save also leaves alone any row another process wrote since.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the transaction or any write fails.
    pub fn save(&self, st: &State) -> Result<(), VivariumError> {
        let snapshot = snapshot(st);
        if self.holds(snapshot.as_ref()) {
            return Ok(());
        }
        self.remember(None);
        let conn = self.conn();
        let tx = conn.unchecked_transaction().map_err(db_err("begin"))?;
        clear(&tx)?;
        write_meta(&tx, st)?;
        write_units(&tx, st)?;
        write_needs(&tx, st)?;
        write_lists(&tx, st)?;
        tx.commit().map_err(db_err("commit"))?;
        self.remember(snapshot);
        Ok(())
    }

    /// Read the exception rows, oldest first: `(at, kind, unit, text)`.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub fn exceptions(&self) -> Result<Vec<(u64, String, String, String)>, VivariumError> {
        let conn = self.conn();
        let mut stmt = conn
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

    /// True when an exception row of exactly `kind` and `unit` exists.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the table cannot be read.
    pub fn has_exception(&self, kind: &str, unit: &str) -> Result<bool, VivariumError> {
        self.conn()
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM exceptions WHERE kind = ?1 AND unit = ?2
                 )",
                params![kind, unit],
                |row| row.get::<_, i64>(0),
            )
            .map(|found| found != 0)
            .map_err(db_err("look for exception"))
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
    let store = PumpStore::open(&home.join(DB_FILENAME))?;
    store
        .conn()
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

/// `st` serialized, to compare against the state the tables hold. A state
/// that cannot be serialized has no snapshot, so its save is never skipped.
fn snapshot(st: &State) -> Option<Vec<u8>> {
    match serde_json::to_vec(st) {
        Ok(bytes) => Some(bytes),
        Err(_) => None,
    }
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
    drop(store.conn().execute(
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
    build_waits::ensure_tables(conn)?;
    step_payload::ensure_table(conn)?;
    job_release::ensure_table(conn)?;
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
/// them, so only a column that is absent is added.
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
        if !has_column(conn, table, column)? {
            conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}"),
                [],
            )
            .map_err(db_err("add column"))?;
        }
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, VivariumError> {
    conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .map_err(db_err("read table columns"))
}
