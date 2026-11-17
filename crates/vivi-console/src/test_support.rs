//! Fixtures shared by the crate's tests: a seeded project on disk, a fake
//! server host, and a run builder.

use std::path::PathBuf;

use rusqlite::{Connection, params};
use tempfile::TempDir;
use vivi_storage::storage::Storage;

use crate::host::{Catalog, ConsoleHost, ProjectRef};

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

/// A project with the full mailspace schema. The schema includes
/// `slot_runs`; `with_slot_runs = false` drops it to stand for a store made
/// before schema 11.
pub fn seeded(with_slot_runs: bool) -> Seeded {
    let (dir, root) = project_without_store();
    let writer = Storage::open_mailspace(&root.join(".vivi")).unwrap();
    let conn = Connection::open(root.join(".vivi/mail.sqlite")).unwrap();
    if !with_slot_runs {
        conn.execute_batch("DROP TABLE slot_runs").unwrap();
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

    /// A catalog that shows this project as `demo`.
    pub fn catalog(&self) -> Catalog {
        Catalog {
            projects: vec![ProjectRef {
                id: "proj-1".into(),
                name: "demo".into(),
                mailspace_root: self.root.clone(),
            }],
        }
    }
}

/// A host that shows a fixed catalog.
pub struct FakeHost {
    pub catalog: Catalog,
}

impl ConsoleHost for FakeHost {
    fn projects(&self) -> Vec<ProjectRef> {
        self.catalog.projects.clone()
    }
}
