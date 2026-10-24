//! Tests for [`crate::pump::report`]: cadence receipts and the missing/unreadable
//! paths.

use tempfile::tempdir;

use crate::pump::cadence::{CadenceState, RoleRun, set_state};
use crate::pump::report::{PumpRead, read};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

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
