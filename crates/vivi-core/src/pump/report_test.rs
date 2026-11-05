//! Tests for [`crate::pump::report`]: cadence receipts, the persisted build
//! waits, and the missing/unreadable paths.

use tempfile::tempdir;

use crate::pump::cadence::{CadenceState, RoleRun, set_state};
use crate::pump::report::{PumpRead, read};
use crate::pump::state::State;
use crate::pump::store::build_waits::{BuildPrerequisite, BuildWait, PrerequisiteState};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn ready(st: State) -> crate::pump::report::Snapshot {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let store = PumpStore::open(&path).unwrap();
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
    let store = PumpStore::open(&path).unwrap();
    store.save(&State::default()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(matches!(read(dir.path()).unwrap(), PumpRead::Ready(_)));
    conn.execute_batch("ROLLBACK").unwrap();
}

fn wait(need: &str, record: &str, handle: &str, state: PrerequisiteState) -> BuildWait {
    BuildWait {
        need: need.into(),
        record: record.into(),
        prerequisites: vec![BuildPrerequisite {
            handle: handle.into(),
            state,
            detail: None,
            outcome: None,
        }],
        observed_at: 1_700_000_000,
    }
}

#[test]
fn persisted_build_waits_are_available_from_the_snapshot() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let store = PumpStore::open(&path).unwrap();
    store.save(&State::default()).unwrap();
    let held = wait("n1", "r1", "a", PrerequisiteState::Held);
    let unlanded = wait("n2", "r2", "b", PrerequisiteState::ClosedUnlanded);
    store.upsert_build_wait(&unlanded).unwrap();
    store.upsert_build_wait(&held).unwrap();

    let PumpRead::Ready(snapshot) = read(dir.path()).unwrap() else {
        panic!("expected a ready snapshot");
    };
    assert_eq!(snapshot.build_waits(), [held.clone(), unlanded.clone()]);
    assert_eq!(snapshot.build_waits()[0].observed_at, 1_700_000_000);
    // The read judged nothing and wrote nothing.
    assert!(snapshot.exception_rows().is_empty());
    assert!(snapshot.cadence_runs().is_empty());
    assert_eq!(store.build_waits().unwrap(), vec![held, unlanded]);
    assert!(store.exceptions().unwrap().is_empty());
}

#[test]
fn a_snapshot_without_the_wait_table_has_no_waits_and_creates_none() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    {
        let store = PumpStore::open(&path).unwrap();
        store.save(&State::default()).unwrap();
    }
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE build_waits")
        .unwrap();
    let PumpRead::Ready(snapshot) = read(dir.path()).unwrap() else {
        panic!("expected a ready snapshot");
    };
    assert!(snapshot.build_waits().is_empty());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'build_waits'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}
