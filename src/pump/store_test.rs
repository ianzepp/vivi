//! Tests for [`crate::pump::store`]: schema, round trip, export, and the
//! exception row.

use tempfile::tempdir;

use crate::pump::state::{Attempt, GateRecord, NeedRecord, Promotion, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

/// A `state.json` written by the live pump, used as the import fixture.
const LIVE_SNAPSHOT: &str = include_str!("state_snapshot.json");

fn sample() -> State {
    let mut st = State::default();
    let mut unit = Unit {
        need: String::from("n1"),
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
        history: vec![Attempt {
            attempt: 1,
            model: Some(String::from("m")),
            why: String::from("timeout"),
            detail: String::from("slow"),
            at: 9,
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
    });
    st.red
        .insert(String::from("crate test"), String::from("u1"));
    st.red_reported = vec![String::from("n1:x")];
    st.merges = 3;
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
    assert_eq!(unit.status, Status::Merged);
    assert_eq!(unit.commit.as_deref(), Some("abc123"));
    assert_eq!(unit.merged_at, Some(12));
    assert_eq!(unit.history.len(), 1);
    assert_eq!(unit.history[0].why, "timeout");
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
