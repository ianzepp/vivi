//! Tests for [`crate::pump::store`]: schema, round trip, export, and the
//! exception row.

use std::fs::File;

use tempfile::tempdir;

use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus};
use crate::pump::state::{Attempt, NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

/// A `state.json` written by the live pump, used as the import fixture.
const LIVE_SNAPSHOT: &str = include_str!("state_snapshot.json");

fn sample() -> State {
    let mut st = State::default();
    let mut unit = Unit {
        need: String::from("n1"),
        target: String::from("tela"),
        title: String::from("one"),
        brief: String::from("do it"),
        write_scope: vec![String::from("src/a")],
        after: vec![String::from("u0")],
        proof: vec![String::from("true")],
        tools: String::from("plain"),
        status: Status::Merged,
        attempts: 2,
        task: Some(String::from("task-1")),
        branch: Some(String::from("unit/u1-a1")),
        model: Some(String::from("m")),
        since: 10,
        since_dispatch: Some(11),
        ready_at: Some(8),
        next_branch_suffix: Some(5),
        pool: Some(String::from("glm")),
        commit: Some(String::from("abc123")),
        merged_at: Some(12),
        outside: vec![String::from("src/b")],
        replaced_by: vec![String::from("u1.1")],
        verdict_text: Some(String::from("ok")),
        kind_reported: true,
        split_fails: 2,
        split_retry_at: Some(99),
        split_reasks: 1,
        history: vec![Attempt {
            attempt: 1,
            model: Some(String::from("m")),
            why: String::from("timeout"),
            detail: String::from("slow"),
            at: 9,
            branch: Some(String::from("unit/u1-a1")),
            task: Some(String::from("t1")),
        }],
        ..Unit::default()
    };
    unit.extra
        .insert(String::from("future"), serde_json::json!(7));
    st.units.insert(String::from("u1"), unit);
    st.order = vec![String::from("u1")];
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            reason: String::from("r"),
            units: vec![String::from("u1")],
            revision: 3,
            thread_sha256: Some(String::from("abc")),
            thread_seen_at: Some(77),
            amendment_pending: true,
            ..NeedRecord::default()
        },
    );
    st.closed_needs = vec![String::from("n1")];
    st.plans_seen = vec![String::from("n1.json")];
    st.merges = 3;
    st.rate_limited_at = Some(1_700_000_123);
    st.extra
        .insert(String::from("future"), serde_json::json!("kept"));
    st
}

#[test]
fn schema_is_versioned_and_a_new_store_is_empty() {
    let dir = tempdir().unwrap();
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(store.is_empty().unwrap());
}

#[test]
fn a_saved_state_round_trips() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut state = sample();
    let mut spec = JobSpec::new(
        String::from("reclaim-default-u1"),
        String::from("reclaim"),
        String::from("hand"),
        String::from("reclaim u1"),
        String::from("cleanup request"),
    );
    spec.local_only = true;
    spec.pool_candidates = vec![String::from("here")];
    spec.payload = serde_json::json!({"request": {"version": 1}});
    crate::pump::jobs::enqueue(&mut state, spec).unwrap();
    let job = state.jobs.get_mut("reclaim-default-u1").unwrap();
    job.status = JobStatus::Finished;
    job.applied_at = Some(43);
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("cleaned 1 slot"),
        receipt: String::from("cleaned lanes slot-1 build 80"),
        at: 42,
    });
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&state).unwrap();
    assert!(!store.is_empty().unwrap());
    let back = store.load().unwrap();
    let unit = &back.units["u1"];
    assert_eq!(unit.target, "tela");
    assert_eq!(unit.status, Status::Merged);
    assert_eq!(unit.commit.as_deref(), Some("abc123"));
    assert_eq!(unit.merged_at, Some(12));
    assert_eq!(unit.ready_at, Some(8));
    assert_eq!(unit.next_branch_suffix, Some(5));
    assert_eq!(unit.split_fails, 2);
    assert_eq!(unit.split_retry_at, Some(99));
    assert_eq!(unit.split_reasks, 1);
    assert!(!unit.extra.contains_key("split_fails"));
    assert!(!unit.extra.contains_key("split_retry_at"));
    assert!(!unit.extra.contains_key("split_reasks"));
    assert_eq!(unit.history.len(), 1);
    assert_eq!(unit.history[0].why, "timeout");
    assert_eq!(unit.history[0].branch.as_deref(), Some("unit/u1-a1"));
    assert_eq!(unit.history[0].task.as_deref(), Some("t1"));
    assert_eq!(unit.extra.get("future").unwrap(), &serde_json::json!(7));
    assert_eq!(back.order, vec![String::from("u1")]);
    assert_eq!(back.needs["n1"].units, vec![String::from("u1")]);
    assert_eq!(back.needs["n1"].revision, 3);
    assert_eq!(back.needs["n1"].thread_sha256.as_deref(), Some("abc"));
    assert_eq!(back.needs["n1"].thread_seen_at, Some(77));
    assert!(back.needs["n1"].amendment_pending);
    assert_eq!(back.closed_needs, vec![String::from("n1")]);
    assert_eq!(back.plans_seen, vec![String::from("n1.json")]);
    assert_eq!(back.merges, 3);
    assert_eq!(back.rate_limited_at, Some(1_700_000_123));
    let job = &back.jobs["reclaim-default-u1"];
    assert!(job.local_only);
    assert_eq!(job.status, JobStatus::Finished);
    assert_eq!(job.applied_at, Some(43));
    assert_eq!(
        job.outcome.as_ref().unwrap().receipt,
        "cleaned lanes slot-1 build 80"
    );
    assert_eq!(
        back.extra.get("future").unwrap(),
        &serde_json::json!("kept")
    );
}

#[test]
fn the_live_state_snapshot_round_trips_through_the_database() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let state: State = serde_json::from_str(LIVE_SNAPSHOT).unwrap();
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&state).unwrap();
    let back = store.load().unwrap();
    assert_eq!(back.units.len(), state.units.len());
    assert_eq!(back.needs.len(), state.needs.len());
    assert_eq!(back.extra, state.extra);
    assert_eq!(back.order, state.order);
    assert_eq!(back.merges, state.merges);
    assert_eq!(back.closed_needs, state.closed_needs);
    let (id, want) = state.units.iter().next().unwrap();
    let got = &back.units[id];
    assert_eq!(got.target, "default");
    assert_eq!(got.status, want.status);
    assert_eq!(got.need, want.need);
    assert_eq!(got.history.len(), want.history.len());
}

#[test]
fn a_version_one_database_gains_the_revision_columns() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE needs (
           handle TEXT PRIMARY KEY, verdict TEXT NOT NULL, reason TEXT NOT NULL,
           units_json TEXT NOT NULL, extra_json TEXT NOT NULL);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(conn);
    let store = PumpStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(store.load().unwrap().needs.is_empty());
}

#[test]
fn a_unit_saved_with_revision_zero_loads_back_with_revision_zero() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    let mut state = State::default();
    state.needs.insert(
        String::from("n1"),
        NeedRecord {
            revision: 4,
            units: vec![String::from("current")],
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from("current"),
        Unit {
            need: String::from("n1"),
            revision: 0,
            ..Unit::default()
        },
    );
    store.save(&state).unwrap();
    drop(store);

    let loaded = PumpStore::open(&path).unwrap().load().unwrap();
    assert_eq!(loaded.units["current"].revision, 0);
}

#[test]
fn a_version_three_database_migrates_units_to_named_targets() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "ALTER TABLE units DROP COLUMN target;
         PRAGMA user_version = 3;",
    )
    .unwrap();
    drop(conn);
    let store = PumpStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(store.load().unwrap().units.is_empty());
}

#[test]
fn a_version_five_database_gains_the_attempt_branch_and_task_columns() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "ALTER TABLE unit_events DROP COLUMN branch;
         ALTER TABLE unit_events DROP COLUMN task;
         PRAGMA user_version = 5;",
    )
    .unwrap();
    drop(conn);
    let mut store = PumpStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    store.save(&sample()).unwrap();
    let attempt = &store.load().unwrap().units["u1"].history[0];
    assert_eq!(attempt.branch.as_deref(), Some("unit/u1-a1"));
    assert_eq!(attempt.task.as_deref(), Some("t1"));
    // Opening again is a no-op: the column ensure is idempotent.
    drop(store);
    let again = PumpStore::open(&path).unwrap();
    assert_eq!(again.load().unwrap().units["u1"].history.len(), 1);
}

#[test]
fn an_attempt_without_a_branch_or_task_keeps_the_old_json_shape() {
    let attempt = Attempt {
        attempt: 1,
        why: String::from("x"),
        ..Attempt::default()
    };
    let text = serde_json::to_string(&attempt).unwrap();
    assert!(!text.contains("branch") && !text.contains("task"), "{text}");
    let back: Attempt = serde_json::from_str(&text).unwrap();
    assert_eq!(back.branch, None);
}

#[test]
fn a_version_four_database_gains_the_need_priority_table() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE need_priority;
         PRAGMA user_version = 4;",
    )
    .unwrap();
    drop(conn);
    let mut store = PumpStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(store.need_priorities().unwrap().is_empty());
    store.set_need_priority("n", 3, 10).unwrap();
    assert_eq!(store.need_priorities().unwrap(), [("n".to_string(), 3)]);
}

#[test]
fn saving_state_leaves_need_priority_alone() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.set_need_priority("n", 2, 10).unwrap();
    store.save(&State::default()).unwrap();
    assert_eq!(store.need_priorities().unwrap(), [("n".to_string(), 2)]);
}

#[test]
fn schema_migration_refuses_to_run_under_an_active_pump_lock() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(conn);

    let lock = File::create(dir.path().join("lock")).unwrap();
    lock.try_lock().unwrap();
    let err = match PumpStore::open(&path) {
        Ok(_) => panic!("schema migration must wait for the active pump"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("another pump owns"), "{err}");
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION - 1);
    drop(lock);

    // A sibling test that spawns a process can hold a dup of this lock for
    // the instant between fork and exec, so allow the release a moment.
    let store = eventually(|| PumpStore::open(&path).ok());
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(crate::pump::run::lock_is_held(dir.path()).unwrap());
    drop(store);
    eventually(|| (!crate::pump::run::lock_is_held(dir.path()).unwrap()).then_some(()));
}

/// Retry `f` for up to two seconds until it returns `Some`.
fn eventually<T>(mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(value) = f() {
            return value;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    f().expect("condition did not hold within two seconds")
}

#[test]
fn the_locked_pump_can_migrate_its_own_store_at_startup() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(conn);

    let lock = File::create(dir.path().join("lock")).unwrap();
    lock.try_lock().unwrap();
    let store = PumpStore::open_for_run(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn a_newer_schema_is_refused_and_left_alone() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    let err = match PumpStore::open(&path) {
        Ok(_) => panic!("a newer schema must be refused"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("newer than this binary"), "{err}");
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 99);
}

#[test]
fn an_unknown_status_survives_the_round_trip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut state = State::default();
    state.units.insert(
        String::from("u9"),
        Unit {
            status: Status::from_name("paused"),
            ..Unit::default()
        },
    );
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&state).unwrap();
    let back = store.load().unwrap();
    assert_eq!(back.units["u9"].status.as_str(), "paused");
}

#[test]
fn an_exception_is_recorded_in_the_database() {
    let dir = tempdir().unwrap();
    crate::pump::store::append_exception(dir.path(), 42, "stuck", "u1", "needs a person");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    let rows = store.exceptions().unwrap();
    assert_eq!(rows[0].0, 42);
    assert_eq!(rows[0].1, "stuck");
    assert_eq!(rows[0].3, "needs a person");
}

#[test]
fn save_keeps_the_append_only_exception_log() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    crate::pump::store::append_exception(dir.path(), 1, "stuck", "u1", "first");
    store.save(&State::default()).unwrap();
    store.save(&sample()).unwrap();
    assert_eq!(store.exceptions().unwrap().len(), 1);
}

#[test]
fn save_replaces_rows_rather_than_accumulating_them() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&sample()).unwrap();
    store.save(&State::default()).unwrap();
    let back = store.load().unwrap();
    assert!(back.units.is_empty());
    assert!(back.needs.is_empty());
    assert_eq!(back.merges, 0);
}

/// The tables of the retired operator request queues (schema 7).
const RETIRED_QUEUES: [&str; 3] = ["requeue_requests", "withdraw_requests", "replan_requests"];

fn table_exists(conn: &rusqlite::Connection, table: &str) -> bool {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    count > 0
}

#[test]
fn a_fresh_store_has_no_operator_request_tables() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    for table in RETIRED_QUEUES {
        assert!(!table_exists(&conn, table), "{table}");
    }
}

/// A schema 7 store, as vivi 18 wrote it, holds the three request queues,
/// each with an unapplied request. Opening it under schema 8 drops them (the
/// operator re-issues the action on the need's pump record), keeps the state,
/// and bumps the version.
#[test]
fn migrating_a_schema_7_store_drops_the_three_request_tables_and_keeps_the_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&sample()).unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE requeue_requests (request_id INTEGER PRIMARY KEY AUTOINCREMENT,
           unit TEXT NOT NULL, attempts INTEGER, fresh INTEGER NOT NULL DEFAULT 0,
           refund INTEGER NOT NULL DEFAULT 0, reason TEXT NOT NULL DEFAULT '',
           requested_at INTEGER NOT NULL, applied_at INTEGER);
         CREATE TABLE withdraw_requests (request_id INTEGER PRIMARY KEY AUTOINCREMENT,
           need TEXT NOT NULL, reason TEXT NOT NULL DEFAULT '',
           requested_at INTEGER NOT NULL, applied_at INTEGER);
         CREATE TABLE replan_requests (request_id INTEGER PRIMARY KEY AUTOINCREMENT,
           need TEXT NOT NULL, force INTEGER NOT NULL DEFAULT 0,
           requested_at INTEGER NOT NULL, applied_at INTEGER);
         INSERT INTO requeue_requests (unit, requested_at) VALUES ('u1', 5);
         INSERT INTO withdraw_requests (need, requested_at) VALUES ('n1', 5);
         INSERT INTO replan_requests (need, requested_at) VALUES ('n1', 5);
         PRAGMA user_version = 7;",
    )
    .unwrap();
    drop(conn);

    let store = PumpStore::open(&path).unwrap();

    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(store.load().unwrap().units.len(), sample().units.len());
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    for table in RETIRED_QUEUES {
        assert!(!table_exists(&conn, table), "{table} was not dropped");
    }
}

#[test]
fn applied_actions_and_the_records_a_need_ran_under_survive_a_state_save() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    let mut st = State::default();
    st.board.actions.insert(
        String::from("letter-1"),
        crate::pump::state::ActionMark {
            record: String::from("r1"),
            need: String::from("n1"),
            kind: String::from("replan"),
            owes: String::from("superseded"),
            detail: String::from("operator replan"),
        },
    );
    st.board
        .records
        .insert(String::from("n1"), String::from("r1"));
    store.save(&st).unwrap();

    let back = store.load().unwrap();

    assert_eq!(back.board.actions, st.board.actions);
    assert_eq!(back.board.records, st.board.records);
}

#[test]
fn retired_unit_and_state_keys_still_load_and_are_written_back() {
    let json =
        r#"{"env_pauses":{"here":{"fails":[1]}},"units":{"u1":{"need":"n","send_reported":true}}}"#;
    let state: State = serde_json::from_str(json).unwrap();
    assert!(state.extra.contains_key("env_pauses"));
    assert!(state.units["u1"].extra.contains_key("send_reported"));
}

/// A store written by a pump before `needs.withdrawn` existed (schema 6, as
/// on hub before 18.1.2) must gain the column on open, not fail every read.
#[test]
fn a_schema_6_store_without_withdrawn_gains_the_column_on_open() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("ALTER TABLE needs DROP COLUMN withdrawn")
        .unwrap();
    conn.pragma_update(None, "user_version", 6).unwrap();
    drop(conn);

    let lock = File::create(dir.path().join("lock")).unwrap();
    lock.try_lock().unwrap();
    let store = PumpStore::open_for_run(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    store.load().expect("needs read after migration");
}
