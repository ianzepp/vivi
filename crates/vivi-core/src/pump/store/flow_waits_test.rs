use super::{FlowWaitRow, WaitingHandle};
use crate::pump::store::{DB_FILENAME, PumpStore};
use tempfile::tempdir;
fn sample(need: &str, record: &str) -> FlowWaitRow {
    FlowWaitRow {
        need: need.into(),
        record: record.into(),
        handles: vec![WaitingHandle {
            handle: "n-prereq".into(),
            state: "open".into(),
            detail: "waiting for review".into(),
        }],
        evaluated_at: 42,
    }
}
#[test]
fn projection_replaces_reconciles_and_survives_state_saves_and_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let store = PumpStore::open(&path).unwrap();
    store.upsert_flow_wait(&sample("n2", "r2")).unwrap();
    store.upsert_flow_wait(&sample("n1", "r1")).unwrap();
    assert_eq!(
        store
            .flow_waits()
            .unwrap()
            .iter()
            .map(|r| r.need.as_str())
            .collect::<Vec<_>>(),
        vec!["n1", "n2"]
    );
    store.upsert_flow_wait(&sample("n1", "r3")).unwrap();
    store.save(&Default::default()).unwrap();
    drop(store);
    let store = PumpStore::open(&path).unwrap();
    assert_eq!(store.flow_waits().unwrap()[0], sample("n1", "r3"));
    store
        .reconcile_flow_waits(&[("n1".into(), "r3".into())])
        .unwrap();
    assert_eq!(store.flow_waits().unwrap().len(), 1);
    store.remove_flow_wait("n1").unwrap();
    assert!(store.flow_waits().unwrap().is_empty());
}
#[test]
fn read_only_missing_projection_is_empty_and_does_not_create_table() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("DROP TABLE flow_waits").unwrap();
    drop(conn);
    let store = PumpStore::open_read_only(&path).unwrap().unwrap();
    assert!(store.flow_waits().unwrap().is_empty());
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='flow_waits'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn read_only_missing_path_stays_missing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("absent.db");
    assert!(PumpStore::open_read_only(&path).unwrap().is_none());
    assert!(!path.exists());
}
