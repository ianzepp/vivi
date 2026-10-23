//! Tests for [`crate::pump::report`]: counts, completion, rendering, and the
//! missing/unreadable paths.

use tempfile::tempdir;

use crate::pump::cadence::{CadenceState, RoleRun, set_state};
use crate::pump::report::{PumpRead, read, render_need, render_state};
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
fn need_summary_counts_and_completion() {
    let snapshot = ready(state());
    let summary = snapshot.need("n1").unwrap();
    assert_eq!(summary.verdict, "units");
    assert_eq!(summary.units_total, 2);
    assert_eq!(summary.counts["merged"], 1);
    assert_eq!(summary.counts["pending"], 1);
    assert!(!summary.complete);
    assert!(snapshot.need("n2").unwrap().complete);
    assert!(snapshot.need("n3").unwrap().complete);
    assert!(snapshot.need("missing").is_none());
}

#[test]
fn render_names_the_registry_state_and_counts() {
    let snapshot = ready(state());
    let text = render_need(&snapshot.need("n1").unwrap(), Some(true));
    assert!(text.contains("verdict units"), "{text}");
    assert!(text.contains("r3"), "{text}");
    assert!(text.contains("2 units: merged=1 pending=1"), "{text}");
    assert!(text.contains("complete no"), "{text}");
    assert!(text.contains("amendment pending"), "{text}");
    assert!(text.contains("registry open"), "{text}");
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
    let snapshot = ready(st);
    assert!(snapshot.need("n1").unwrap().complete);
}

#[test]
fn state_summary_counts_complete_stuck_and_inflight() {
    let mut st = state();
    st.units
        .insert(String::from("u4"), unit("n1", Status::Stuck));
    st.units
        .insert(String::from("u5"), unit("n1", Status::Inflight));
    let summary = ready(st).summary();
    assert_eq!(summary.complete, 2);
    assert_eq!(summary.stuck, 1);
    assert_eq!(summary.inflight, 1);
    let text = render_state(&summary, 2);
    assert!(text.contains("working"), "{text}");
    assert!(text.contains("complete-but-open 2"), "{text}");
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
            branch: None,
            read_only: true,
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
