//! Tests for [`super::PumpStore::load_snapshot`]: the state, the exception
//! rows and the queued build waits out of one consistent read.

use rusqlite::{Connection, OpenFlags};
use tempfile::{TempDir, tempdir};

use super::PumpStore;
use crate::pump::state::State;
use crate::pump::store::DB_FILENAME;
use crate::pump::store::append_exception;
use crate::pump::store::build_waits::{BuildPrerequisite, BuildWait, PrerequisiteState};

fn path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join(DB_FILENAME)
}

fn wait(need: &str, record: &str, handles: &[&str]) -> BuildWait {
    BuildWait {
        need: need.into(),
        record: record.into(),
        prerequisites: handles
            .iter()
            .map(|handle| BuildPrerequisite {
                handle: (*handle).into(),
                state: PrerequisiteState::Held,
                detail: None,
                outcome: None,
            })
            .collect(),
        observed_at: 1_700_000_000,
    }
}

fn table_exists(path: &std::path::Path, table: &str) -> bool {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open");
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .expect("count")
        > 0
}

#[test]
fn a_snapshot_carries_state_exceptions_and_waits_from_one_read() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    let mut st = State::default();
    st.merges = 4;
    store.save(&st).expect("save");
    append_exception(dir.path(), 10, "build-wait", "r1/n1/a", "held");
    store
        .upsert_build_wait(&wait("n2", "r2", &["b"]))
        .expect("n2");
    store
        .upsert_build_wait(&wait("n1", "r1", &["a"]))
        .expect("n1");

    let snapshot = store.load_snapshot().expect("snapshot");
    assert_eq!(snapshot.state.merges, 4);
    assert_eq!(snapshot.exceptions.len(), 1);
    assert_eq!(snapshot.exceptions[0].1, "build-wait");
    let needs: Vec<&str> = snapshot.waits.iter().map(|row| row.need.as_str()).collect();
    assert_eq!(needs, ["n1", "n2"]);
    assert_eq!(snapshot.waits[0].record, "r1");
    assert_eq!(snapshot.waits[0].prerequisites[0].handle, "a");
    assert_eq!(snapshot.waits[0].observed_at, 1_700_000_000);
}

#[test]
fn a_snapshot_leaves_no_read_transaction_open() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    store.save(&State::default()).expect("save");
    store.load_snapshot().expect("first");
    // A second snapshot would fail on a still-open transaction.
    store.load_snapshot().expect("second");
    store.save(&State::default()).expect("save after reads");
}

#[test]
fn a_snapshot_skips_another_connections_uncommitted_waits() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    store.save(&State::default()).expect("save");
    let writer = Connection::open(path(&dir)).expect("writer");
    writer.execute_batch("BEGIN IMMEDIATE").expect("begin");
    writer
        .execute(
            "INSERT INTO build_waits (need, record, prerequisites_json, observed_at)
             VALUES ('n9', 'r9', '[]', 1)",
            [],
        )
        .expect("insert");
    assert!(store.load_snapshot().expect("snapshot").waits.is_empty());
    writer.execute_batch("ROLLBACK").expect("rollback");
    assert_eq!(store.build_waits().expect("after").len(), 0);
}

#[test]
fn a_snapshot_of_a_database_without_the_wait_table_reads_empty_and_creates_none() {
    let dir = tempdir().expect("tempdir");
    {
        let store = PumpStore::open(&path(&dir)).expect("open");
        store.save(&State::default()).expect("save");
        store
            .conn()
            .execute("DROP TABLE build_waits", [])
            .expect("drop");
        assert_eq!(store.schema_version().expect("version"), 8);
    }
    let store = PumpStore::open_read_only(&path(&dir))
        .expect("read-only")
        .expect("some");
    let snapshot = store.load_snapshot().expect("snapshot");
    assert!(snapshot.waits.is_empty());
    assert!(snapshot.exceptions.is_empty());
    drop(store);
    assert!(!table_exists(&path(&dir), "build_waits"));
    assert_eq!(
        PumpStore::open_read_only(&path(&dir))
            .expect("again")
            .map(|opened| opened.schema_version().expect("version")),
        Some(8)
    );
}

#[test]
fn reading_a_snapshot_writes_no_exception_and_keeps_every_wait() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    store.save(&State::default()).expect("save");
    let recorded = wait("n1", "r1", &["a", "b"]);
    store.upsert_build_wait(&recorded).expect("wait");

    let snapshot = store.load_snapshot().expect("snapshot");
    assert_eq!(snapshot.waits, vec![recorded.clone()]);
    assert_eq!(store.build_waits().expect("after"), vec![recorded]);
    assert!(store.exceptions().expect("exceptions").is_empty());
    assert!(!dir.path().join("exceptions.txt").exists());
}
