use super::*;
use crate::test_support::{project_without_store, seeded};

#[test]
fn a_project_without_a_store_opens_as_none() {
    let (_dir, root) = project_without_store();
    assert!(ReadStore::open(&root).unwrap().is_none());
}

#[test]
fn the_connection_refuses_every_write() {
    let store = seeded(true);
    let read = ReadStore::open(&store.root).unwrap().unwrap();
    let conn = read.connection();
    let query_only: i64 = conn
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .unwrap();
    assert_eq!(query_only, 1);
    assert!(conn.execute("DELETE FROM slot_runs", []).is_err());
    assert!(
        conn.execute_batch("CREATE TABLE intruder (x INTEGER)")
            .is_err()
    );
}

#[test]
fn the_store_is_never_changed_by_reading() {
    let store = seeded(true);
    let db = store.root.join(".vivi/mail.sqlite");
    let before = std::fs::read(&db).unwrap();
    let read = ReadStore::open(&store.root).unwrap().unwrap();
    read.read(|conn| {
        assert!(has_table(conn, "slot_runs")?);
        assert!(!has_table(conn, "nope")?);
        Ok(())
    })
    .unwrap();
    drop(read);
    // A writer is still attached (the WAL carries its changes), so compare
    // the main file only.
    assert_eq!(before, std::fs::read(&db).unwrap());
}

#[test]
fn a_store_without_slot_runs_reports_it() {
    let store = seeded(false);
    let read = ReadStore::open(&store.root).unwrap().unwrap();
    read.read(|conn| {
        assert!(!has_table(conn, "slot_runs")?);
        Ok(())
    })
    .unwrap();
}
