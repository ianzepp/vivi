//! Fixtures shared by the crate's tests: a seeded project on disk, a fake
//! server host, and a run builder.

use std::path::PathBuf;

use rusqlite::{Connection, params};
use tempfile::TempDir;
use vivi_storage::storage::Storage;

use crate::host::{ConsoleHost, Principal, ProjectRef};

/// The agreed `slot_runs` DDL (storage schema 11), copied verbatim from the
/// slot index seat's contract.
///
/// To be replaced by `vivi_storage::storage::slot_runs::SLOT_RUNS_DDL` once
/// that lands in vivi-storage.
pub const SLOT_RUNS_FIXTURE_DDL: &str = "
CREATE TABLE IF NOT EXISTS slot_runs (
  job TEXT PRIMARY KEY,
  task_message_id TEXT NOT NULL,
  need TEXT,
  pool TEXT NOT NULL,
  slot TEXT NOT NULL,
  machine TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL DEFAULT '',
  kind TEXT,
  target TEXT,
  branch TEXT NOT NULL DEFAULT '',
  start_sha TEXT NOT NULL DEFAULT '',
  budget_s INTEGER,
  read_only INTEGER NOT NULL DEFAULT 0,
  dispatched_at INTEGER NOT NULL,
  collected_at INTEGER,
  outcome TEXT,
  push TEXT,
  sha TEXT,
  agent_rc INTEGER,
  agent_s INTEGER,
  check_rc INTEGER,
  check_s INTEGER,
  job_s INTEGER,
  agent_cpu_s INTEGER,
  check_cpu_s INTEGER,
  landing TEXT,
  landing_at INTEGER,
  landing_reason TEXT
);
CREATE INDEX IF NOT EXISTS slot_runs_slot_idx ON slot_runs(pool, slot, dispatched_at);
CREATE INDEX IF NOT EXISTS slot_runs_need_idx ON slot_runs(need, dispatched_at);
CREATE INDEX IF NOT EXISTS slot_runs_outcome_idx ON slot_runs(outcome, collected_at);
CREATE INDEX IF NOT EXISTS slot_runs_dispatched_idx ON slot_runs(dispatched_at);
CREATE INDEX IF NOT EXISTS slot_runs_collected_idx ON slot_runs(collected_at);
CREATE INDEX IF NOT EXISTS slot_runs_task_idx ON slot_runs(task_message_id);
";

/// One `slot_runs` row to insert.
pub struct SeedRun<'a> {
    pub job: &'a str,
    pub task: &'a str,
    pub need: Option<&'a str>,
    pub pool: &'a str,
    pub slot: &'a str,
    pub machine: &'a str,
    pub model: &'a str,
    pub kind: Option<&'a str>,
    pub budget_s: Option<i64>,
    pub dispatched_at: i64,
    pub collected_at: Option<i64>,
    pub outcome: Option<&'a str>,
    pub agent_rc: Option<i64>,
    pub check_rc: Option<i64>,
    pub job_s: Option<i64>,
    pub landing: Option<&'a str>,
}

impl Default for SeedRun<'_> {
    fn default() -> Self {
        Self {
            job: "job-0",
            task: "msg_0123456789abcdef0123",
            need: None,
            pool: "main",
            slot: "slot-1",
            machine: "studio-01",
            model: "model-a",
            kind: Some("build"),
            budget_s: Some(3300),
            dispatched_at: 0,
            collected_at: None,
            outcome: None,
            agent_rc: None,
            check_rc: None,
            job_s: None,
            landing: None,
        }
    }
}

/// A project on disk with a live writer, like a running server.
pub struct Seeded {
    pub _dir: TempDir,
    pub root: PathBuf,
    pub conn: Connection,
    // Keeps the WAL index file in place, as a running vivi does.
    _writer: Storage,
}

const CONFIG: &str = "name = \"demo\"\n\n[pools.main]\nprovider = \"local\"\nslots = 4\n";

/// A project with config and no store.
pub fn project_without_store() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join(".vivi")).unwrap();
    std::fs::write(root.join(".vivi/mailspace.toml"), CONFIG).unwrap();
    (dir, root)
}

/// A project with a full mailspace schema, and the `slot_runs` fixture when
/// `with_slot_runs` is true.
pub fn seeded(with_slot_runs: bool) -> Seeded {
    let (dir, root) = project_without_store();
    let writer = Storage::open_mailspace(&root.join(".vivi")).unwrap();
    let conn = Connection::open(root.join(".vivi/mail.sqlite")).unwrap();
    if with_slot_runs {
        conn.execute_batch(SLOT_RUNS_FIXTURE_DDL).unwrap();
    }
    Seeded {
        _dir: dir,
        root,
        conn,
        _writer: writer,
    }
}

impl Seeded {
    /// Insert one run.
    pub fn insert(&self, run: &SeedRun<'_>) {
        self.conn
            .execute(
                "INSERT INTO slot_runs (job, task_message_id, need, pool, slot, machine, \
                 model, kind, budget_s, dispatched_at, collected_at, outcome, agent_rc, \
                 check_rc, job_s, landing) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    run.job,
                    run.task,
                    run.need,
                    run.pool,
                    run.slot,
                    run.machine,
                    run.model,
                    run.kind,
                    run.budget_s,
                    run.dispatched_at,
                    run.collected_at,
                    run.outcome,
                    run.agent_rc,
                    run.check_rc,
                    run.job_s,
                    run.landing,
                ],
            )
            .unwrap();
    }

    /// A principal that may read this project as `demo`.
    pub fn principal(&self) -> Principal {
        Principal {
            name: "alice".into(),
            projects: vec![ProjectRef {
                id: "proj-1".into(),
                name: "demo".into(),
                mailspace_root: self.root.clone(),
            }],
        }
    }
}

/// A host with a fixed key table.
pub struct FakeHost {
    pub keys: Vec<(String, Principal)>,
}

impl ConsoleHost for FakeHost {
    fn authenticate(&self, key: &str) -> Option<Principal> {
        self.keys
            .iter()
            .find(|(known, _)| known == key)
            .map(|(_, principal)| principal.clone())
    }
}
