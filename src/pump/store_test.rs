//! Tests for [`crate::pump::store`]: schema, round trip, export, and the
//! exception row.

use std::fs::File;

use tempfile::tempdir;

use crate::pump::state::{
    Attempt, EnvPause, GateRecord, NeedRecord, Promotion, State, Status, TargetState, Unit,
};
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
        pool: Some(String::from("glm")),
        commit: Some(String::from("abc123")),
        merged_at: Some(12),
        outside: vec![String::from("src/b")],
        replaced_by: vec![String::from("u1.1")],
        verdict_text: Some(String::from("ok")),
        kind_reported: true,
        send_reported: true,
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
    st.target_states.insert(
        String::from("radix"),
        TargetState {
            merges: 3,
            baseline: Some(vec![String::from("radix failure")]),
            ..TargetState::default()
        },
    );
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
    st.gates.push(GateRecord {
        tip: String::from("tip"),
        at: 13,
        merges: 2,
        names: Some(vec![String::from("crate test")]),
        new: None,
    });
    st.baseline = Some(vec![String::from("old")]);
    st.closed_needs = vec![String::from("n1")];
    st.plans_seen = vec![String::from("n1.json")];
    st.promotions.push(Promotion {
        tip: String::from("tip"),
        at: 14,
        failing: None,
    });
    st.red
        .insert(String::from("crate test"), String::from("u1"));
    st.red_reported = vec![String::from("n1:x")];
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
    let state = sample();
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&state).unwrap();
    assert!(!store.is_empty().unwrap());
    let back = store.load().unwrap();
    let unit = &back.units["u1"];
    assert_eq!(unit.target, "tela");
    assert_eq!(back.target_states["radix"].merges, 3);
    assert_eq!(
        back.target_states["radix"].baseline,
        Some(vec![String::from("radix failure")])
    );
    assert_eq!(unit.status, Status::Merged);
    assert_eq!(unit.commit.as_deref(), Some("abc123"));
    assert_eq!(unit.merged_at, Some(12));
    assert!(unit.send_reported);
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
    assert_eq!(back.gates[0].names.as_ref().unwrap()[0], "crate test");
    assert_eq!(back.gates[0].new, None);
    assert_eq!(back.baseline.as_ref().unwrap()[0], "old");
    assert_eq!(back.closed_needs, vec![String::from("n1")]);
    assert_eq!(back.plans_seen, vec![String::from("n1.json")]);
    assert_eq!(back.promotions[0].at, 14);
    assert_eq!(back.red["crate test"], "u1");
    assert_eq!(back.red_reported, vec![String::from("n1:x")]);
    assert_eq!(back.merges, 3);
    assert_eq!(back.rate_limited_at, Some(1_700_000_123));
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
    assert_eq!(back.gates.len(), state.gates.len());
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
fn export_writes_the_state_json_shape() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&sample()).unwrap();
    let out = dir.path().join("export.json");
    store.export_json(&out).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("\"u1\""), "{text}");
    let back = State::load(&out).unwrap();
    assert_eq!(back.units["u1"].status, Status::Merged);
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

#[test]
fn requeue_requests_survive_a_state_save() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store
        .add_requeue_request("u1", Some(0), true, true, "operator asked", 5)
        .unwrap();
    store.save(&State::default()).unwrap();
    let pending = store.pending_requeue_requests().unwrap();
    assert_eq!(pending.len(), 1);
    let request = &pending[0];
    assert_eq!(request.unit, "u1");
    assert_eq!(request.attempts, Some(0));
    assert!(request.fresh);
    assert!(request.refund);
    assert_eq!(request.reason, "operator asked");
    store.mark_requeue_applied(request.request_id, 6).unwrap();
    assert!(store.pending_requeue_requests().unwrap().is_empty());
}

#[test]
fn replan_requests_survive_a_state_save() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.add_replan_request("n1", false, 5).unwrap();
    store.save(&State::default()).unwrap();

    let pending = store.pending_replan_requests().unwrap();
    assert_eq!(pending.len(), 1);
    let request = &pending[0];
    assert_eq!(request.need, "n1");
    assert!(!request.force);
    store.mark_replan_applied(request.request_id, 6).unwrap();
    assert!(store.pending_replan_requests().unwrap().is_empty());
}

#[test]
fn env_pauses_round_trip_through_pump_db() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let empty = PumpStore::open(&path).unwrap();
    assert!(empty.load().unwrap().env_pauses.is_empty());
    drop(empty);

    let mut state = State::default();
    state.env_pauses.insert(
        String::from("here"),
        EnvPause {
            fails: vec![1_700_000_000, 1_700_000_060],
            evidence: String::from("`pi` not found: bash: line 1: pi: command not found"),
            probe: Some(String::from("command -v pi")),
            paused_at: Some(1_700_000_120),
        },
    );
    state.env_pauses.insert(
        String::from("glm"),
        EnvPause {
            fails: vec![1_700_000_000],
            evidence: String::from("git index.lock held"),
            ..EnvPause::default()
        },
    );

    let mut store = PumpStore::open(&path).unwrap();
    store.save(&state).unwrap();
    let back = store.load().unwrap();
    assert_eq!(back.env_pauses, state.env_pauses);
    let here = &back.env_pauses["here"];
    assert_eq!(here.fails, vec![1_700_000_000, 1_700_000_060]);
    assert_eq!(here.probe.as_deref(), Some("command -v pi"));
    assert_eq!(here.paused_at, Some(1_700_000_120));
    assert_eq!(back.env_pauses["glm"].probe, None);
    assert_eq!(back.env_pauses["glm"].paused_at, None);
}
