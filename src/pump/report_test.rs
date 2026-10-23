//! Tests for [`crate::pump::report`]: counts, completion, rendering, and the
//! missing/unreadable paths.

use tempfile::tempdir;

use crate::pump::cadence::{CadenceState, RoleRun, set_state};
use crate::pump::report::{PumpRead, read};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn unit(need: &str, status: Status) -> Unit {
    Unit {
        need: need.to_owned(),
        status,
        since: 10,
        ..Unit::default()
    }
}

fn state() -> State {
    let mut st = State::default();
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u1"), String::from("u2")],
            revision: 3,
            amendment_pending: true,
            ..NeedRecord::default()
        },
    );
    st.units
        .insert(String::from("u1"), unit("n1", Status::Merged));
    st.units
        .insert(String::from("u2"), unit("n1", Status::Pending));
    st.needs.insert(
        String::from("n2"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u3")],
            ..NeedRecord::default()
        },
    );
    st.units
        .insert(String::from("u3"), unit("n2", Status::Merged));
    st.needs.insert(
        String::from("n3"),
        NeedRecord {
            verdict: String::from("delivered"),
            ..NeedRecord::default()
        },
    );
    st
}

fn ready(st: State) -> crate::pump::report::Snapshot {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&st).unwrap();
    match read(dir.path()).unwrap() {
        PumpRead::Ready(snapshot) => snapshot,
        _ => panic!("expected a ready snapshot"),
    }
}

#[test]
fn complete_needs_are_the_delivered_or_fully_satisfied() {
    let complete = ready(state()).complete_needs();
    assert!(!complete.contains("n1"));
    assert!(complete.contains("n2"));
    assert!(complete.contains("n3"));
}

#[test]
fn replaced_units_delegate_to_their_successors() {
    let mut st = State::default();
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u1")],
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        String::from("u1"),
        Unit {
            need: String::from("n1"),
            status: Status::Replaced,
            replaced_by: vec![String::from("u1.1")],
            ..Unit::default()
        },
    );
    st.units
        .insert(String::from("u1.1"), unit("n1", Status::Merged));
    assert!(ready(st).complete_needs().contains("n1"));
}

#[test]
fn cadence_run_receipts_are_available_to_the_board() {
    let mut st = State::default();
    let mut cadence = CadenceState::default();
    cadence.runs.insert(
        String::from("digest"),
        RoleRun {
            job: String::from("cadence-digest-1"),
            task: String::from("abcd1234"),
            started_at: 100,
            ended_at: Some(200),
            outcome: Some(String::from("success")),
            attempt: 0,
            retry_at: None,
        },
    );
    set_state(&mut st, &cadence);

    let runs = ready(st).cadence_runs();
    assert_eq!(runs["digest"].started_at, 100);
    assert_eq!(runs["digest"].ended_at, Some(200));
    assert_eq!(runs["digest"].outcome.as_deref(), Some("success"));
}

#[test]
fn a_missing_database_is_not_local() {
    let dir = tempdir().unwrap();
    assert!(matches!(read(dir.path()).unwrap(), PumpRead::Missing));
}

#[test]
fn an_unreadable_database_is_reported_not_fatal() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(DB_FILENAME), b"not sqlite").unwrap();
    assert!(matches!(read(dir.path()).unwrap(), PumpRead::Unreadable(_)));
}

#[test]
fn an_older_or_newer_schema_is_reported_not_read() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    assert!(matches!(read(dir.path()).unwrap(), PumpRead::Unreadable(_)));
}

#[test]
fn a_writer_does_not_block_a_reader() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&State::default()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(matches!(read(dir.path()).unwrap(), PumpRead::Ready(_)));
    conn.execute_batch("ROLLBACK").unwrap();
}
