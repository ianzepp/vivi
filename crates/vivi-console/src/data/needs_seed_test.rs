//! Fixtures for the needs pages: need letters in a seeded mailspace store and
//! flow rows in a seeded pump store.

use rusqlite::{Connection, params};
use vivi_core::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use vivi_core::pump::store::flow_links::SliceLink;
use vivi_core::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

use crate::test_support::Seeded;

const GRAPH: &str = "graph-backlog";

/// One need to seed.
pub(crate) struct NeedSeed<'a> {
    pub handle: &'a str,
    pub subject: &'a str,
    pub state: &'a str,
    pub created_at: &'a str,
    pub body: &'a str,
}

pub(crate) const NEED_BODY: &str = "+++\ndone_when = [\"HIR lowers & | ^\", \"register test passes\"]\ntarget = \"vivi\"\n+++\n\nThe brief prose.";

impl Default for NeedSeed<'_> {
    fn default() -> Self {
        Self {
            handle: "a1f3c2d0",
            subject: "clif: lower tuple returns",
            state: "open",
            created_at: "2026-11-14T08:00:00+00:00",
            body: NEED_BODY,
        }
    }
}

/// The content id the seed gives a need.
pub(crate) fn content_of(handle: &str) -> String {
    format!("content-{handle}")
}

fn eml(subject: &str, body: &str, extra: &str) -> String {
    format!(
        "From: pump@x\nTo: a@x\nSubject: {subject}\n{extra}Content-Type: text/plain; \
         charset=utf-8\nContent-Transfer-Encoding: 7bit\n\n{body}\n"
    )
}

fn put_letter(store: &Seeded, content: &str, date: &str, subject: &str, raw: &str) {
    let relpath = format!("blobs/{content}.eml");
    let dir = store.root.join(".vivi/blobs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(store.root.join(".vivi").join(&relpath), raw).unwrap();
    store
        .conn
        .execute(
            "INSERT INTO blobs (content_id, blob_relpath, byte_size, parsed_at) \
             VALUES (?1, ?2, ?3, '2026-11-14T00:00:00Z')",
            params![content, relpath, i64::try_from(raw.len()).unwrap()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO message_metadata \
             (content_id, date, from_addr, to_addr, cc_addr, bcc_addr, subject) \
             VALUES (?1, ?2, 'pump@x', 'a@x', '', '', ?3)",
            params![content, date, subject],
        )
        .unwrap();
}

/// Seed a need: its node in the backlog graph, its letter and its message.
pub(crate) fn seed_need(store: &Seeded, need: &NeedSeed<'_>) {
    store
        .conn
        .execute(
            "INSERT OR IGNORE INTO work_graphs (handle, code, status, current_revision, \
             created_at, updated_at) VALUES (?1, 'backlog', 'open', 1, 'now', 'now')",
            [GRAPH],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO work_graph_nodes (handle, graph_handle, source_id, label, state, \
             kind, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 'need', ?6, ?6)",
            params![
                format!("node-{}", need.handle),
                GRAPH,
                need.handle,
                need.subject,
                need.state,
                need.created_at
            ],
        )
        .unwrap();
    let content = content_of(need.handle);
    put_letter(
        store,
        &content,
        need.created_at,
        need.subject,
        &eml(need.subject, need.body, "X-Vivi-Kind: need\n"),
    );
    store
        .conn
        .execute(
            "INSERT INTO messages (message_id, account, content_id, local_role, discovered_at, \
             updated_at) VALUES (?1, 'pump', ?2, 'needs', 'now', 'now')",
            params![format!("msg_{}0000000000", need.handle), content],
        )
        .unwrap();
}

/// Seed a reply linked below `parent`. An event reply is a lifecycle record.
pub(crate) fn seed_reply(
    store: &Seeded,
    parent: &str,
    id: &str,
    date: &str,
    body: &str,
    event: bool,
) {
    let content = format!("content-{id}");
    let extra = if event { "X-Vivi-Event: marked\n" } else { "" };
    put_letter(
        store,
        &content,
        date,
        &format!("Re: {id}"),
        &eml(&format!("Re: {id}"), body, extra),
    );
    store
        .conn
        .execute(
            "INSERT INTO mailspace_links (child_content_id, parent_content_id, source) \
             VALUES (?1, ?2, 'captured')",
            params![content, parent],
        )
        .unwrap();
}

/// A pump store beside the seeded project, with a writer kept open so the WAL
/// index file exists the way it does under a running pump.
pub(crate) struct PumpSeed {
    pub store: PumpStore,
    pub writer: Connection,
}

/// Declare a pump in the project's config and create its store.
pub(crate) fn seed_pump(store: &Seeded) -> PumpSeed {
    let config = store.root.join(".vivi/mailspace.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("{text}\n[pump]\n")).unwrap();
    let home = store.root.join(".vivi/pump");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join(DB_FILENAME);
    let pump = PumpStore::open(&path).unwrap();
    let writer = Connection::open(&path).unwrap();
    writer
        .query_row("SELECT count(*) FROM units", [], |row| row.get::<_, i64>(0))
        .unwrap();
    PumpSeed {
        store: pump,
        writer,
    }
}

impl PumpSeed {
    pub(crate) fn run(&self, need: &str, run: u32, kind: &str, entry: &str, state: &str) {
        self.store
            .insert_run(&RunRow {
                need: need.into(),
                run,
                flow: kind.into(),
                flow_version: 2,
                kind: kind.into(),
                entry: entry.into(),
                state: state.into(),
            })
            .unwrap();
    }

    pub(crate) fn step(&self, need: &str, run: u32, node: &str, task: &str, state: StepState) {
        self.step_verdict(need, run, node, task, state, None);
    }

    pub(crate) fn step_verdict(
        &self,
        need: &str,
        run: u32,
        node: &str,
        task: &str,
        state: StepState,
        verdict: Option<&str>,
    ) {
        self.store
            .insert_step(
                need,
                &StepRow {
                    run,
                    node: node.into(),
                    task: task.into(),
                    state,
                    verdict: verdict.map(Into::into),
                },
            )
            .unwrap();
    }

    pub(crate) fn job(
        &self,
        need: &str,
        run: u32,
        node: &str,
        task: &str,
        job: &str,
        attempt: u32,
    ) {
        self.store
            .insert_step_job(
                need,
                &StepJobRow {
                    job: job.into(),
                    run,
                    node: node.into(),
                    task: task.into(),
                    attempt,
                },
            )
            .unwrap();
    }

    /// A slice step with its scope and link.
    pub(crate) fn slice(&self, need: &str, run: u32, id: &str, fanout: &str, scope: &[&str]) {
        let task = format!("task-{id}");
        self.step(need, run, &format!("slice:{id}"), &task, StepState::Running);
        let scope: Vec<String> = scope.iter().map(ToString::to_string).collect();
        self.store.insert_slice_scope(&task, run, &scope).unwrap();
        self.store
            .link_slice(&SliceLink {
                task,
                need: need.into(),
                run,
                fanout: Some(fanout.into()),
                deps: Vec::new(),
            })
            .unwrap();
    }

    pub(crate) fn branch(&self, need: &str, branch: &str) {
        self.store
            .upsert_need_branch(&NeedBranchRow {
                need: need.into(),
                branch: branch.into(),
                base: Some("abc1234".into()),
                status: "open".into(),
                run: 1,
                target: "default".into(),
                priors: Vec::new(),
            })
            .unwrap();
    }

    pub(crate) fn rung(
        &self,
        task: &str,
        rung: &str,
        started: i64,
        finished: Option<i64>,
        rc: Option<i64>,
    ) {
        self.writer
            .execute(
                "INSERT INTO rung_runs (task, rung, started_at, finished_at, rc) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![task, rung, started, finished, rc],
            )
            .unwrap();
    }

    pub(crate) fn plan(&self, need: &str, revision: i64, verdict: &str, reason: &str, at: i64) {
        self.writer
            .execute(
                "INSERT INTO plan_revisions (need, revision, verdict, reason, body_json, created_at) \
                 VALUES (?1, ?2, ?3, ?4, '{}', ?5)",
                params![need, revision, verdict, reason, at],
            )
            .unwrap();
    }
}
