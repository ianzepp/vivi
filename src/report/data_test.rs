use super::*;

use std::fs;

#[test]
fn report_read_returns_unavailable_when_wal_shm_is_missing_without_creating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".vivi");
    drop(Storage::open_mailspace(&dir).unwrap());
    let db = dir.join("mail.sqlite");
    let shm = dir.join("mail.sqlite-shm");
    let wal = dir.join("mail.sqlite-wal");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(conn);
    let _ = fs::remove_file(&shm);
    let _ = fs::remove_file(&wal);
    let before = fs::read(&db).unwrap();

    let error = read(tmp.path()).err().unwrap().to_string();
    assert!(error.contains("mail.sqlite-shm"), "{error}");
    assert!(
        !shm.exists(),
        "the report reader created the missing sidecar"
    );
    assert_eq!(fs::read(&db).unwrap(), before);
}
