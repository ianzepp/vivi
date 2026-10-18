//! The pump's SQLite store (`pump.db`).
//!
//! `pump.db` is the pump's live memory. The in-memory [`State`] remains the
//! decision model: one [`PumpStore::load`] at startup, one
//! [`PumpStore::save`] per save point, each in a single transaction. The
//! `state.json` shape is kept as the import/export format so a predecessor
//! pump can still read a converted file.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::Path;
use std::time::Duration;

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};

use crate::error::VivariumError;

mod priority;
mod schema;
use schema::{newer_schema, schema_version, take_migration_lock};

use super::state::{Attempt, GateRecord, NeedRecord, Promotion, State, Status, Unit};

/// The schema version written to `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = 5;

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
  at INTEGER NOT NULL
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
CREATE TABLE IF NOT EXISTS promotions (tip TEXT PRIMARY KEY, at INTEGER NOT NULL);
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
            gates: load_gates(&self.conn)?,
            closed_needs: load_strings(&self.conn, "closed_needs", "need")?,
            plans_seen: load_strings(&self.conn, "plans_seen", "name")?,
            promotions: load_promotions(&self.conn)?,
            red: load_red(&self.conn)?,
            red_reported: load_strings(&self.conn, "red_reported", "value")?,
            ..State::default()
        };
        load_meta(&self.conn, &mut st)?;
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
        write_gates(&tx, st)?;
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
        ("units", "revision", "INTEGER NOT NULL DEFAULT 1"),
        ("units", "target", "TEXT NOT NULL DEFAULT 'default'"),
    ] {
        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {ddl}");
        if conn.execute(&sql, []).is_err() {
            // The column already exists (fresh schema).
        }
    }
    Ok(())
}

fn clear(tx: &Transaction<'_>) -> Result<(), VivariumError> {
    for table in [
        "needs",
        "plan_revisions",
        "units",
        "unit_edges",
        "unit_events",
        "unit_order",
        "merges",
        "gates",
        "gate_failures",
        "closed_needs",
        "plans_seen",
        "promotions",
        "red",
        "red_reported",
        "meta",
    ] {
        tx.execute(&format!("DELETE FROM {table}"), [])
            .map_err(db_err("clear"))?;
    }
    Ok(())
}

fn write_meta(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    put(tx, "initialized", "1")?;
    put(tx, "merges", &st.merges.to_string())?;
    put(tx, "baseline", &json_or_null(&st.baseline)?)?;
    put(tx, "last_gate", &json_or_null(&st.last_gate)?)?;
    put(tx, "board", &json_or_null(&st.board)?)?;
    put(tx, "extra", &json_or_null(&st.extra)?)?;
    put(tx, "target_states", &json_or_null(&st.target_states)?)?;
    Ok(())
}

fn write_units(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    for (position, id) in st.order.iter().enumerate() {
        let position = i64::try_from(position).unwrap_or(0);
        tx.execute(
            "INSERT OR IGNORE INTO unit_order (position, unit) VALUES (?1, ?2)",
            params![position, id],
        )
        .map_err(db_err("write unit order"))?;
    }
    for (id, unit) in &st.units {
        write_unit(tx, id, unit)?;
    }
    Ok(())
}

fn write_unit(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    insert_unit_row(tx, id, unit)?;
    write_unit_edges(tx, id, unit)?;
    write_unit_events(tx, id, unit)?;
    write_merge_row(tx, id, unit)
}

fn insert_unit_row(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT INTO units (id, need, target, title, brief, write_scope_json, after_json, proof_json,
            tools, status, attempts, task, branch, model, since, since_dispatch, pool,
            commit_sha, merged_at, outside_json, replaced_by_json, verdict_text,
            kind_reported, extra_json, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
        params![
            id,
            &unit.need,
            &unit.target,
            &unit.title,
            &unit.brief,
            json(&unit.write_scope)?,
            json(&unit.after)?,
            json(&unit.proof)?,
            &unit.tools,
            unit.status.as_str(),
            i64::from(unit.attempts),
            &unit.task,
            &unit.branch,
            &unit.model,
            i64::try_from(unit.since).unwrap_or(0),
            unit.since_dispatch.map(|v| i64::try_from(v).unwrap_or(0)),
            &unit.pool,
            &unit.commit,
            unit.merged_at.map(|v| i64::try_from(v).unwrap_or(0)),
            json(&unit.outside)?,
            json(&unit.replaced_by)?,
            &unit.verdict_text,
            i64::from(unit.kind_reported),
            json(&unit.extra)?,
            i64::try_from(unit.revision).unwrap_or(0),
        ],
    )
    .map_err(db_err("write unit"))?;
    Ok(())
}

fn write_unit_edges(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    for after in &unit.after {
        tx.execute(
            "INSERT OR IGNORE INTO unit_edges (unit, after_unit) VALUES (?1, ?2)",
            params![id, after],
        )
        .map_err(db_err("write unit edge"))?;
    }
    Ok(())
}

fn write_unit_events(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    for attempt in &unit.history {
        tx.execute(
            "INSERT INTO unit_events (unit, attempt, model, why, detail, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                i64::from(attempt.attempt),
                &attempt.model,
                &attempt.why,
                &attempt.detail,
                i64::try_from(attempt.at).unwrap_or(0),
            ],
        )
        .map_err(db_err("write unit event"))?;
    }
    Ok(())
}

fn write_merge_row(tx: &Transaction<'_>, id: &str, unit: &Unit) -> Result<(), VivariumError> {
    if let (Some(commit), Some(at)) = (unit.commit.as_deref(), unit.merged_at) {
        tx.execute(
            "INSERT INTO merges (unit, commit_sha, at, outside_json) VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                commit,
                i64::try_from(at).unwrap_or(0),
                json(&unit.outside)?
            ],
        )
        .map_err(db_err("write merge"))?;
    }
    Ok(())
}

fn write_needs(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    for (handle, need) in &st.needs {
        tx.execute(
            "INSERT INTO needs (handle, verdict, reason, units_json, revision,
                thread_sha256, thread_seen_at, amendment_pending, extra_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                handle,
                &need.verdict,
                &need.reason,
                json(&need.units)?,
                i64::try_from(need.revision).unwrap_or(0),
                &need.thread_sha256,
                need.thread_seen_at.map(|at| i64::try_from(at).unwrap_or(0)),
                i64::from(need.amendment_pending),
                "{}",
            ],
        )
        .map_err(db_err("write need"))?;
    }
    Ok(())
}

fn write_gates(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    for gate in &st.gates {
        tx.execute(
            "INSERT INTO gates (tip, at, merges, names_json, new_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &gate.tip,
                i64::try_from(gate.at).unwrap_or(0),
                i64::try_from(gate.merges).unwrap_or(0),
                json_or_null(&gate.names)?,
                json_or_null(&gate.new)?,
            ],
        )
        .map_err(db_err("write gate"))?;
        let gate_id = tx.last_insert_rowid();
        for (kind, names) in [("names", &gate.names), ("new", &gate.new)] {
            for name in names.iter().flatten() {
                tx.execute(
                    "INSERT OR IGNORE INTO gate_failures (gate_id, name, kind)
                     VALUES (?1, ?2, ?3)",
                    params![gate_id, name, kind],
                )
                .map_err(db_err("write gate failure"))?;
            }
        }
    }
    Ok(())
}

fn write_lists(tx: &Transaction<'_>, st: &State) -> Result<(), VivariumError> {
    for need in &st.closed_needs {
        tx.execute(
            "INSERT OR IGNORE INTO closed_needs (need) VALUES (?1)",
            params![need],
        )
        .map_err(db_err("write closed need"))?;
    }
    for name in &st.plans_seen {
        tx.execute(
            "INSERT OR IGNORE INTO plans_seen (name) VALUES (?1)",
            params![name],
        )
        .map_err(db_err("write plan seen"))?;
    }
    for promotion in &st.promotions {
        tx.execute(
            "INSERT OR REPLACE INTO promotions (tip, at) VALUES (?1, ?2)",
            params![promotion.tip, i64::try_from(promotion.at).unwrap_or(0)],
        )
        .map_err(db_err("write promotion"))?;
    }
    for (name, unit) in &st.red {
        tx.execute(
            "INSERT OR REPLACE INTO red (name, unit) VALUES (?1, ?2)",
            params![name, unit],
        )
        .map_err(db_err("write red"))?;
    }
    for value in &st.red_reported {
        tx.execute(
            "INSERT OR IGNORE INTO red_reported (value) VALUES (?1)",
            params![value],
        )
        .map_err(db_err("write red reported"))?;
    }
    Ok(())
}

fn load_meta(conn: &Connection, st: &mut State) -> Result<(), VivariumError> {
    st.merges = get(conn, "merges")?.map_or(0, |v| v.parse().unwrap_or(0));
    st.baseline = get_json(conn, "baseline")?;
    st.last_gate = get_json(conn, "last_gate")?;
    st.board = get_json(conn, "board")?;
    st.extra = get_json(conn, "extra")?;
    st.target_states = get_json(conn, "target_states")?;
    Ok(())
}

fn load_units(conn: &Connection) -> Result<BTreeMap<String, Unit>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, need, target, title, brief, write_scope_json, after_json, proof_json,
                tools, status, attempts, task, branch, model, since, since_dispatch, pool,
                commit_sha, merged_at, outside_json, replaced_by_json, verdict_text,
                kind_reported, extra_json, revision
             FROM units",
        )
        .map_err(db_err("read units"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Unit {
                    need: row.get(1)?,
                    target: row.get(2)?,
                    title: row.get(3)?,
                    brief: row.get(4)?,
                    write_scope: from_json(row.get(5)?),
                    after: from_json(row.get(6)?),
                    proof: from_json(row.get(7)?),
                    tools: row.get(8)?,
                    status: Status::from_name(&row.get::<_, String>(9)?),
                    attempts: from_u32(row.get(10)?),
                    task: row.get(11)?,
                    branch: row.get(12)?,
                    model: row.get(13)?,
                    since: from_u64(row.get(14)?),
                    since_dispatch: row.get::<_, Option<i64>>(15)?.map(from_u64),
                    pool: row.get(16)?,
                    commit: row.get(17)?,
                    merged_at: row.get::<_, Option<i64>>(18)?.map(from_u64),
                    outside: from_json(row.get(19)?),
                    replaced_by: from_json(row.get(20)?),
                    verdict_text: row.get(21)?,
                    kind_reported: row.get::<_, i64>(22)? != 0,
                    held_on: None,
                    extra: from_json(row.get(23)?),
                    revision: from_u64(row.get(24)?),
                    history: Vec::new(),
                },
            ))
        })
        .map_err(db_err("read units"))?;
    let mut units = BTreeMap::new();
    for row in rows {
        let (id, unit) = row.map_err(db_err("read unit"))?;
        units.insert(id, unit);
    }
    load_events(conn, &mut units)?;
    Ok(units)
}

fn load_events(conn: &Connection, units: &mut BTreeMap<String, Unit>) -> Result<(), VivariumError> {
    let mut stmt = conn
        .prepare("SELECT unit, attempt, model, why, detail, at FROM unit_events ORDER BY event_id")
        .map_err(db_err("read unit events"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Attempt {
                    attempt: from_u32(row.get(1)?),
                    model: row.get(2)?,
                    why: row.get(3)?,
                    detail: row.get(4)?,
                    at: from_u64(row.get(5)?),
                },
            ))
        })
        .map_err(db_err("read unit events"))?;
    for row in rows {
        let (unit, attempt) = row.map_err(db_err("read unit event"))?;
        if let Some(unit) = units.get_mut(&unit) {
            unit.history.push(attempt);
        }
    }
    Ok(())
}

fn load_needs(conn: &Connection) -> Result<BTreeMap<String, NeedRecord>, VivariumError> {
    let mut stmt = conn
        .prepare(
            "SELECT handle, verdict, reason, units_json, revision,
                thread_sha256, thread_seen_at, amendment_pending
             FROM needs",
        )
        .map_err(db_err("read needs"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                NeedRecord {
                    verdict: row.get(1)?,
                    reason: row.get(2)?,
                    units: from_json(row.get(3)?),
                    revision: from_u64(row.get(4)?),
                    thread_sha256: row.get(5)?,
                    thread_seen_at: row.get::<_, Option<i64>>(6)?.map(from_u64),
                    amendment_pending: row.get::<_, i64>(7)? != 0,
                },
            ))
        })
        .map_err(db_err("read needs"))?;
    let mut needs = BTreeMap::new();
    for row in rows {
        let (handle, need) = row.map_err(db_err("read need"))?;
        needs.insert(handle, need);
    }
    Ok(needs)
}

fn load_order(conn: &Connection) -> Result<Vec<String>, VivariumError> {
    let mut stmt = conn
        .prepare("SELECT unit FROM unit_order ORDER BY position")
        .map_err(db_err("read unit order"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_err("read unit order"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read unit order"))
}

fn load_gates(conn: &Connection) -> Result<Vec<GateRecord>, VivariumError> {
    let mut stmt = conn
        .prepare("SELECT tip, at, merges, names_json, new_json FROM gates ORDER BY gate_id")
        .map_err(db_err("read gates"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(GateRecord {
                tip: row.get(0)?,
                at: from_u64(row.get(1)?),
                merges: from_u64(row.get(2)?),
                names: from_opt_json(row.get(3)?),
                new: from_opt_json(row.get(4)?),
            })
        })
        .map_err(db_err("read gates"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read gates"))
}

fn load_promotions(conn: &Connection) -> Result<Vec<Promotion>, VivariumError> {
    let mut stmt = conn
        .prepare("SELECT tip, at FROM promotions ORDER BY at")
        .map_err(db_err("read promotions"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Promotion {
                tip: row.get(0)?,
                at: from_u64(row.get(1)?),
            })
        })
        .map_err(db_err("read promotions"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read promotions"))
}

fn load_red(conn: &Connection) -> Result<BTreeMap<String, String>, VivariumError> {
    let mut stmt = conn
        .prepare("SELECT name, unit FROM red")
        .map_err(db_err("read red"))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))
        .map_err(db_err("read red"))?;
    let mut red = BTreeMap::new();
    for row in rows {
        let (name, unit) = row.map_err(db_err("read red"))?;
        red.insert(name, unit);
    }
    Ok(red)
}

fn load_strings(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<Vec<String>, VivariumError> {
    let mut stmt = conn
        .prepare(&format!("SELECT {column} FROM {table}"))
        .map_err(db_err("read list"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_err("read list"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(db_err("read list"))
}

fn put(tx: &Transaction<'_>, key: &str, value: &str) -> Result<(), VivariumError> {
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map_err(db_err("write meta"))?;
    Ok(())
}

fn get(conn: &Connection, key: &str) -> Result<Option<String>, VivariumError> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(db_err("read meta"))
}

fn get_json<T: Default + serde::de::DeserializeOwned>(
    conn: &Connection,
    key: &str,
) -> Result<T, VivariumError> {
    match get(conn, key)? {
        Some(text) => serde_json::from_str(&text)
            .map_err(|err| VivariumError::Message(format!("pump.db meta {key}: {err}"))),
        None => Ok(T::default()),
    }
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, VivariumError> {
    serde_json::to_string(value).map_err(|err| VivariumError::Message(err.to_string()))
}

fn json_or_null<T: serde::Serialize>(value: &T) -> Result<String, VivariumError> {
    json(value)
}

fn from_json<T: Default + serde::de::DeserializeOwned>(text: String) -> T {
    serde_json::from_str(&text).unwrap_or_default()
}

fn from_opt_json<T: serde::de::DeserializeOwned>(text: Option<String>) -> Option<T> {
    text.and_then(|text| match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(_) => None,
    })
}

fn from_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn from_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

fn db_err<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> VivariumError {
    move |err| VivariumError::Other(format!("pump.db {what}: {err}"))
}
