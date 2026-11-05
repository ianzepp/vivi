//! Tests for the `build_waits` table methods and the exception identity lookup
//! in [`super`].

use rusqlite::{Connection, OpenFlags};
use tempfile::{TempDir, tempdir};

use super::{BuildPrerequisite, BuildWait, PrerequisiteState};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore, append_exception};

fn prerequisite(handle: &str, state: PrerequisiteState) -> BuildPrerequisite {
    BuildPrerequisite {
        handle: handle.into(),
        state,
        detail: None,
        outcome: None,
    }
}

fn wait(need: &str, record: &str, handles: &[&str]) -> BuildWait {
    BuildWait {
        need: need.into(),
        record: record.into(),
        prerequisites: handles
            .iter()
            .map(|handle| prerequisite(handle, PrerequisiteState::Open))
            .collect(),
        observed_at: 1_700_000_000,
    }
}

fn path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join(DB_FILENAME)
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
fn a_typed_wait_round_trips() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    let mut recorded = wait("n1", "r1", &["a", "b"]);
    recorded.prerequisites[0].detail = Some("waiting on the gate".into());
    recorded.prerequisites[1].state = PrerequisiteState::ClosedUnlanded;
    recorded.prerequisites[1].outcome = Some("failed".into());
    store.upsert_build_wait(&recorded).expect("upsert");
    assert_eq!(store.build_waits().expect("read"), vec![recorded]);
}

#[test]
fn a_second_upsert_replaces_the_needs_wait() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    store
        .upsert_build_wait(&wait("n1", "r1", &["a"]))
        .expect("first");
    let mut replacement = wait("n1", "r2", &["b", "c"]);
    replacement.observed_at = 1_700_000_500;
    store.upsert_build_wait(&replacement).expect("replacement");
    assert_eq!(store.build_waits().expect("read"), vec![replacement]);
}

#[test]
fn removing_a_wait_drops_only_that_need() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    store
        .upsert_build_wait(&wait("n1", "r1", &["a"]))
        .expect("n1");
    store
        .upsert_build_wait(&wait("n2", "r2", &["b"]))
        .expect("n2");
    store.remove_build_wait("n1").expect("remove");
    assert_eq!(
        store.build_waits().expect("read"),
        vec![wait("n2", "r2", &["b"])]
    );
    store.remove_build_wait("n1").expect("remove again");
    assert_eq!(store.build_waits().expect("read").len(), 1);
}

#[test]
fn waits_survive_reopen_and_list_in_need_order() {
    let dir = tempdir().expect("tempdir");
    {
        let store = PumpStore::open(&path(&dir)).expect("open");
        store
            .upsert_build_wait(&wait("n3", "r3", &["c"]))
            .expect("n3");
        store
            .upsert_build_wait(&wait("n1", "r1", &["a"]))
            .expect("n1");
        store
            .upsert_build_wait(&wait("n2", "r2", &["b"]))
            .expect("n2");
    }
    let store = PumpStore::open(&path(&dir)).expect("reopen");
    let needs: Vec<String> = store
        .build_waits()
        .expect("read")
        .into_iter()
        .map(|row| row.need)
        .collect();
    assert_eq!(needs, ["n1", "n2", "n3"]);
}

#[test]
fn a_state_save_keeps_wait_rows() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    let recorded = wait("n1", "r1", &["a"]);
    store.upsert_build_wait(&recorded).expect("upsert");
    store.save(&State::default()).expect("save");
    store.save(&State::default()).expect("save again");
    assert_eq!(store.build_waits().expect("read"), vec![recorded]);
}

#[test]
fn exception_lookup_matches_the_exact_kind_and_unit() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    append_exception(dir.path(), 10, "build-wait", "r1/n1/a", "held");
    append_exception(dir.path(), 11, "build-wait", "r1/n1/b", "closed");
    assert!(
        store
            .has_exception("build-wait", "r1/n1/a")
            .expect("lookup")
    );
    assert!(
        !store
            .has_exception("build-wait", "r1/n1/c")
            .expect("missing")
    );
    assert!(!store.has_exception("other", "r1/n1/a").expect("kind"));
    assert!(!store.has_exception("build-wait", "r1").expect("prefix"));
}

#[test]
fn a_malformed_wait_row_stays_an_error() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&path(&dir)).expect("open");
    store
        .conn()
        .execute(
            "INSERT INTO build_waits (need, record, prerequisites_json, observed_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["n1", "r1", "not json", 1_i64],
        )
        .expect("insert malformed");
    assert!(store.build_waits().is_err());
}

#[test]
fn a_same_version_database_without_the_table_reads_empty_and_stays_unchanged() {
    let dir = tempdir().expect("tempdir");
    {
        let store = PumpStore::open(&path(&dir)).expect("open");
        store
            .conn()
            .execute("DROP TABLE build_waits", [])
            .expect("drop");
        assert_eq!(store.schema_version().expect("version"), 8);
    }
    let store = PumpStore::open_read_only(&path(&dir))
        .expect("read-only")
        .expect("some");
    assert!(store.build_waits().expect("empty").is_empty());
    drop(store);
    assert!(!table_exists(&path(&dir), "build_waits"));
    assert_eq!(
        PumpStore::open_read_only(&path(&dir))
            .expect("again")
            .map(|s| s.schema_version().expect("version")),
        Some(8)
    );
}
