use rusqlite::Connection;

use super::read::queries;
use super::{PoolEvent, SLOT_RUNS_DDL, SlotRun, apply_event, apply_event_or_warn};
use crate::storage::schema::{STORAGE_SCHEMA_VERSION, ensure_schema};
use crate::storage::{MailspaceEventInput, Storage};

const DISPATCHED_AT: &str = "2026-11-01T10:00:00+00:00";
const COLLECTED_AT: &str = "2026-11-01T10:05:00+00:00";
const DISPATCHED_EPOCH: i64 = 1_793_527_200;
const COLLECTED_EPOCH: i64 = 1_793_527_500;

fn dispatched(job: &str, slot: &str) -> String {
    serde_json::json!({
        "job": job, "pool": "lanes", "slot": slot, "machine": "studio", "model": "m/ok",
        "need": "need-1", "kind": "hand", "target": "app", "from": "main",
        "branch": "factory/x", "start": "abc123", "budget": 1800, "at": DISPATCHED_EPOCH,
        "read_only": false,
    })
    .to_string()
}

fn collected(job: &str, slot: &str) -> String {
    serde_json::json!({
        "job": job, "pool": "lanes", "slot": slot, "outcome": "finished",
        "push": "ok", "sha": "def456", "agent_rc": "0", "agent_s": "240",
        "check_rc": "0", "check_s": "30", "job_s": "290", "agent_cpu_s": "", "extra": "x",
    })
    .to_string()
}

/// A store, and the events the tests write: each one is appended to the log
/// and folded into `slot_runs`, as `note_message_with_reply` does.
struct World {
    _tmp: tempfile::TempDir,
    storage: Storage,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::open_mailspace(tmp.path()).unwrap();
        // The events name a task that was never ingested: these tests are
        // about the index, not about letters.
        storage
            .conn
            .borrow()
            .execute_batch("PRAGMA foreign_keys = OFF")
            .unwrap();
        Self { _tmp: tmp, storage }
    }

    fn record(&self, event_type: &str, note: &str, at: &str) {
        let input = MailspaceEventInput {
            command: format!("pool {event_type}"),
            event_type: event_type.into(),
            actor_identity: None,
            account: "hand".into(),
            message_id: "task-1".into(),
            content_id: "c".into(),
            from_role: None,
            to_role: None,
            from_identity: None,
            to_identity: None,
            subject: "s".into(),
            note: Some(note.into()),
        };
        self.storage.append_mailspace_event_at(&input, at).unwrap();
        let mut conn = self.storage.conn.borrow_mut();
        let tx = conn.transaction().unwrap();
        let event = PoolEvent {
            event_type,
            note: Some(note),
            message_id: "task-1",
            occurred_at: at,
        };
        apply_event(&tx, &event).unwrap();
        tx.commit().unwrap();
    }

    fn all(&self) -> Vec<SlotRun> {
        self.storage.recent_slot_runs(0, 100).unwrap()
    }
}

#[test]
fn a_dispatch_inserts_a_row_with_the_dispatch_columns() {
    let world = World::new();
    world.record(
        "pool_dispatched",
        &dispatched("j1", "slot-a"),
        DISPATCHED_AT,
    );

    let run = world.storage.slot_run("j1").unwrap().unwrap();

    assert_eq!(run.task_message_id, "task-1");
    assert_eq!((run.pool.as_str(), run.slot.as_str()), ("lanes", "slot-a"));
    assert_eq!(
        (run.machine.as_str(), run.model.as_str()),
        ("studio", "m/ok")
    );
    assert_eq!(run.need.as_deref(), Some("need-1"));
    assert_eq!(run.kind.as_deref(), Some("hand"));
    assert_eq!(run.target.as_deref(), Some("app"));
    assert_eq!(
        (run.branch.as_str(), run.start_sha.as_str()),
        ("factory/x", "abc123")
    );
    assert_eq!(run.budget_s, Some(1800));
    assert_eq!(run.dispatched_at, DISPATCHED_EPOCH);
    assert!(!run.read_only);
    assert_eq!(run.collected_at, None);
    assert_eq!(run.landing, None);
    assert_eq!(world.storage.open_slot_runs().unwrap().len(), 1);
}

#[test]
fn a_collect_fills_the_result_columns_and_stamps_collected_at_from_the_event() {
    let world = World::new();
    world.record(
        "pool_dispatched",
        &dispatched("j1", "slot-a"),
        DISPATCHED_AT,
    );
    world.record("pool_collected", &collected("j1", "slot-a"), COLLECTED_AT);

    let run = world.storage.slot_run("j1").unwrap().unwrap();

    assert_eq!(run.collected_at, Some(COLLECTED_EPOCH));
    assert_eq!(run.dispatched_at, DISPATCHED_EPOCH);
    assert_eq!(run.outcome.as_deref(), Some("finished"));
    assert_eq!(
        (run.push.as_deref(), run.sha.as_deref()),
        (Some("ok"), Some("def456"))
    );
    assert_eq!((run.agent_rc, run.agent_s), (Some(0), Some(240)));
    assert_eq!(
        (run.check_rc, run.check_s, run.job_s),
        (Some(0), Some(30), Some(290))
    );
    assert_eq!(run.agent_cpu_s, None, "an empty string is NULL");
    assert_eq!(
        run.need.as_deref(),
        Some("need-1"),
        "dispatch columns survive"
    );
    assert!(world.storage.open_slot_runs().unwrap().is_empty());
}

#[test]
fn a_collect_without_a_dispatch_inserts_a_row_dated_at_the_collect() {
    let world = World::new();
    world.record("pool_collected", &collected("j9", "slot-b"), COLLECTED_AT);

    let run = world.storage.slot_run("j9").unwrap().unwrap();

    assert_eq!((run.pool.as_str(), run.slot.as_str()), ("lanes", "slot-b"));
    assert_eq!(run.task_message_id, "task-1");
    assert_eq!(run.dispatched_at, COLLECTED_EPOCH);
    assert_eq!(run.collected_at, Some(COLLECTED_EPOCH));
    assert_eq!(run.outcome.as_deref(), Some("finished"));
}

#[test]
fn a_malformed_note_writes_the_event_and_no_row() {
    let world = World::new();
    world.record("pool_dispatched", "not json", DISPATCHED_AT);
    world.record("pool_dispatched", r#"{"job":"j1"}"#, DISPATCHED_AT);
    world.record("pool_collected", r#"["x"]"#, COLLECTED_AT);
    world.record("pool_collected", r#"{"outcome":"finished"}"#, COLLECTED_AT);

    assert!(world.all().is_empty());
    let events = world
        .storage
        .list_mailspace_events_of_types(&["pool_dispatched", "pool_collected"])
        .unwrap();
    assert_eq!(events.len(), 4);
}

#[test]
fn a_failed_index_write_on_the_live_path_keeps_the_event() {
    let world = World::new();
    world
        .storage
        .conn
        .borrow()
        .execute_batch("DROP TABLE slot_runs")
        .unwrap();
    let note = dispatched("j1", "radix-1");
    let input = MailspaceEventInput {
        command: "pool dispatched".into(),
        event_type: "pool_dispatched".into(),
        actor_identity: None,
        account: "hand".into(),
        message_id: "task-1".into(),
        content_id: "c".into(),
        from_role: None,
        to_role: None,
        from_identity: None,
        to_identity: None,
        subject: "s".into(),
        note: Some(note.clone()),
    };
    world
        .storage
        .append_mailspace_event_at(&input, DISPATCHED_AT)
        .unwrap();
    let mut conn = world.storage.conn.borrow_mut();
    let tx = conn.transaction().unwrap();
    let event = PoolEvent {
        event_type: "pool_dispatched",
        note: Some(&note),
        message_id: "task-1",
        occurred_at: DISPATCHED_AT,
    };
    assert!(
        apply_event(&tx, &event).is_err(),
        "the strict form reports it"
    );
    apply_event_or_warn(&tx, &event);
    tx.commit().unwrap();
    drop(conn);
    let events = world
        .storage
        .list_mailspace_events_of_types(&["pool_dispatched"])
        .unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn other_event_types_change_nothing() {
    let world = World::new();
    world.record("absorbed", &dispatched("j1", "slot-a"), DISPATCHED_AT);
    assert!(world.all().is_empty());
}

#[test]
fn rebuild_equals_the_incremental_rows_and_is_idempotent() {
    let world = World::new();
    world.record(
        "pool_dispatched",
        &dispatched("j1", "slot-a"),
        DISPATCHED_AT,
    );
    world.record("pool_collected", &collected("j1", "slot-a"), COLLECTED_AT);
    world.record("pool_dispatched", &dispatched("j2", "slot-b"), COLLECTED_AT);
    world.record("pool_collected", &collected("j3", "slot-c"), COLLECTED_AT);
    world.record("pool_dispatched", "garbage", COLLECTED_AT);
    let incremental = world.all();
    assert_eq!(incremental.len(), 3);

    assert_eq!(world.storage.rebuild_slot_runs().unwrap(), 3);
    assert_eq!(world.all(), incremental);
    assert_eq!(world.storage.rebuild_slot_runs().unwrap(), 3);
    assert_eq!(world.all(), incremental);
}

const LANDED_AT: &str = "2026-11-01T10:09:00+00:00";
const LANDED_EPOCH: i64 = 1_793_527_740;

fn landed(job: &str, landing: &str, extra: serde_json::Value) -> String {
    let mut note = serde_json::json!({ "job": job, "landing": landing });
    for (key, value) in extra.as_object().unwrap() {
        note[key] = value.clone();
    }
    note.to_string()
}

/// A world with `j1` dispatched and collected, then one landing recorded.
fn landed_world(landing: &str, extra: serde_json::Value) -> (World, SlotRun) {
    let world = World::new();
    world.record(
        "pool_dispatched",
        &dispatched("j1", "slot-a"),
        DISPATCHED_AT,
    );
    world.record("pool_collected", &collected("j1", "slot-a"), COLLECTED_AT);
    world.record("pool_landed", &landed("j1", landing, extra), LANDED_AT);
    let run = world.storage.slot_run("j1").unwrap().unwrap();
    (world, run)
}

#[test]
fn a_landed_or_raced_landing_sets_the_columns_and_no_reason() {
    for word in ["landed", "raced"] {
        let (_world, run) = landed_world(word, serde_json::json!({"base": "a", "tip": "b"}));
        assert_eq!(run.landing.as_deref(), Some(word));
        assert_eq!(run.landing_at, Some(LANDED_EPOCH));
        assert_eq!(run.landing_reason, None);
        assert_eq!(run.collected_at, Some(COLLECTED_EPOCH), "collect untouched");
    }
}

#[test]
fn a_red_landing_keeps_the_failing_command_as_its_reason() {
    let (_world, run) = landed_world(
        "red",
        serde_json::json!({"command": "cargo test --workspace", "tail": "boom"}),
    );
    assert_eq!(run.landing.as_deref(), Some("red"));
    assert_eq!(run.landing_at, Some(LANDED_EPOCH));
    assert_eq!(
        run.landing_reason.as_deref(),
        Some("cargo test --workspace")
    );
}

#[test]
fn a_conflict_landing_keeps_the_files_one_per_line() {
    let (_world, run) = landed_world(
        "conflict",
        serde_json::json!({"files": ["src/a.rs", "src/b.rs"], "command": "ignored"}),
    );
    assert_eq!(run.landing.as_deref(), Some("conflict"));
    assert_eq!(run.landing_reason.as_deref(), Some("src/a.rs\nsrc/b.rs"));
}

#[test]
fn a_landing_with_no_row_or_a_bad_word_changes_nothing() {
    let world = World::new();
    world.record(
        "pool_landed",
        &landed("ghost", "landed", serde_json::json!({})),
        LANDED_AT,
    );
    assert!(world.all().is_empty());
    world.record(
        "pool_dispatched",
        &dispatched("j1", "slot-a"),
        DISPATCHED_AT,
    );
    world.record(
        "pool_landed",
        &landed("j1", "merged", serde_json::json!({})),
        LANDED_AT,
    );
    world.record("pool_landed", r#"{"landing":"landed"}"#, LANDED_AT);
    let run = world.storage.slot_run("j1").unwrap().unwrap();
    assert_eq!(
        (run.landing, run.landing_at, run.landing_reason),
        (None, None, None)
    );
}

#[test]
fn rebuild_replays_landings() {
    let (world, run) = landed_world("red", serde_json::json!({"command": "make check"}));
    let before = world.all();
    assert_eq!(run.landing.as_deref(), Some("red"));

    assert_eq!(world.storage.rebuild_slot_runs().unwrap(), 1);

    assert_eq!(world.all(), before);
    let again = world.storage.slot_run("j1").unwrap().unwrap();
    assert_eq!(again.landing_reason.as_deref(), Some("make check"));
    assert_eq!(again.landing_at, Some(LANDED_EPOCH));
}

#[test]
fn rebuild_drops_rows_that_no_event_backs() {
    let world = World::new();
    world.record(
        "pool_dispatched",
        &dispatched("j1", "slot-a"),
        DISPATCHED_AT,
    );
    world
        .storage
        .conn
        .borrow()
        .execute("DELETE FROM mailspace_events", [])
        .unwrap();

    assert_eq!(world.storage.rebuild_slot_runs().unwrap(), 0);
    assert!(world.all().is_empty());
}

#[test]
fn the_reads_filter_and_order_as_documented() {
    let world = World::new();
    world.record(
        "pool_dispatched",
        &dispatched("j1", "slot-a"),
        DISPATCHED_AT,
    );
    let second = dispatched("j2", "slot-a").replace("1793527200", "1793527300");
    world.record("pool_dispatched", &second, DISPATCHED_AT);
    world.record(
        "pool_dispatched",
        &dispatched("j3", "slot-b"),
        DISPATCHED_AT,
    );
    world.record("pool_collected", &collected("j1", "slot-a"), COLLECTED_AT);

    let history = world
        .storage
        .slot_run_history("lanes", "slot-a", 10)
        .unwrap();
    let jobs: Vec<&str> = history.iter().map(|r| r.job.as_str()).collect();
    assert_eq!(jobs, ["j2", "j1"]);
    assert_eq!(
        world
            .storage
            .slot_run_history("lanes", "slot-a", 1)
            .unwrap()
            .len(),
        1
    );
    assert!(
        world
            .storage
            .slot_run_history("other", "slot-a", 10)
            .unwrap()
            .is_empty()
    );

    let open = world.storage.open_slot_runs().unwrap();
    let open: Vec<&str> = open.iter().map(|r| r.job.as_str()).collect();
    assert_eq!(open, ["j3", "j2"]);

    assert_eq!(
        world.storage.slot_runs_for_need("need-1", 2).unwrap().len(),
        2
    );
    assert!(
        world
            .storage
            .slot_runs_for_need("need-2", 2)
            .unwrap()
            .is_empty()
    );
    let recent = world.storage.recent_slot_runs(1_793_527_250, 10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].job, "j2");
}

#[test]
fn every_read_is_answered_from_an_index() {
    let world = World::new();
    let conn = world.storage.conn.borrow();
    for (name, sql, count) in queries() {
        let args: Vec<i64> = vec![1; count];
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {}", super::read::select(sql)))
            .unwrap()
            .query_map(rusqlite::params_from_iter(args), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            !plan.iter().any(|step| step.contains("SCAN slot_runs")),
            "{name} scans the table: {plan:?}"
        );
        assert!(
            plan.iter().any(|step| step.contains("slot_runs")),
            "{name} reads another table: {plan:?}"
        );
    }
}

#[test]
fn a_store_stamped_at_schema_10_gains_the_table_on_open() {
    let tmp = tempfile::tempdir().unwrap();
    drop(Storage::open_mailspace(tmp.path()).unwrap());
    let db = tmp.path().join("mail.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TABLE slot_runs;
         UPDATE storage_metadata SET value = '10' WHERE key = 'schema_version';",
    )
    .unwrap();
    drop(conn);

    let storage = Storage::open_mailspace(tmp.path()).unwrap();

    assert!(storage.open_slot_runs().unwrap().is_empty());
    let conn = Connection::open(&db).unwrap();
    let version: String = conn
        .query_row(
            "SELECT value FROM storage_metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, STORAGE_SCHEMA_VERSION);
    assert_eq!(STORAGE_SCHEMA_VERSION, "12");
}

#[test]
fn the_ddl_is_safe_to_run_twice() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE storage_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .unwrap();
    ensure_schema(&conn).unwrap();
    conn.execute_batch(SLOT_RUNS_DDL).unwrap();
    conn.execute_batch(SLOT_RUNS_DDL).unwrap();
}
