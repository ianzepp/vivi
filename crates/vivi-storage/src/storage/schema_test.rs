use rusqlite::Connection;

use super::{STORAGE_SCHEMA_VERSION, ensure_schema};

fn store_at(version: &str) -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::tempdir().unwrap();
    let conn = Connection::open(tmp.path().join("mail.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE storage_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO storage_metadata (key, value) VALUES ('schema_version', ?1)",
        rusqlite::params![version],
    )
    .unwrap();
    (tmp, conn)
}

fn marker(conn: &Connection) -> String {
    conn.query_row(
        "SELECT value FROM storage_metadata WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn table_count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn a_newer_store_is_refused_and_its_marker_left_alone() {
    let newer = (STORAGE_SCHEMA_VERSION.parse::<u64>().unwrap() + 1).to_string();
    let (_tmp, conn) = store_at(&newer);
    let tables_before = table_count(&conn);

    let error = ensure_schema(&conn).unwrap_err().to_string();

    assert_eq!(
        error,
        format!(
            "mailspace store schema version {newer} is newer than this vivi supports \
             (schema version {STORAGE_SCHEMA_VERSION}); upgrade vivi to open it"
        )
    );
    assert_eq!(marker(&conn), newer);
    assert_eq!(table_count(&conn), tables_before, "no DDL may run");
}

#[test]
fn a_newer_store_is_refused_through_the_public_open() {
    let newer = (STORAGE_SCHEMA_VERSION.parse::<u64>().unwrap() + 5).to_string();
    let (tmp, conn) = store_at(&newer);
    drop(conn);

    let error = crate::storage::Storage::open_mailspace(tmp.path())
        .err()
        .expect("a newer store must not open")
        .to_string();

    assert!(error.contains("upgrade vivi"), "{error}");
    let conn = Connection::open(tmp.path().join("mail.sqlite")).unwrap();
    assert_eq!(marker(&conn), newer);
}

#[test]
fn a_store_at_this_version_opens() {
    let (_tmp, conn) = store_at(STORAGE_SCHEMA_VERSION);
    ensure_schema(&conn).unwrap();
    assert_eq!(marker(&conn), STORAGE_SCHEMA_VERSION);
}

#[test]
fn an_older_store_migrates_forward_to_this_version() {
    let (_tmp, conn) = store_at("6");
    ensure_schema(&conn).unwrap();
    assert_eq!(marker(&conn), STORAGE_SCHEMA_VERSION);
    let graphs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'work_graphs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(graphs, 1);
}

#[test]
fn a_nonnumeric_marker_keeps_the_repair_pass() {
    let (_tmp, conn) = store_at("garbled");
    ensure_schema(&conn).unwrap();
    assert_eq!(marker(&conn), STORAGE_SCHEMA_VERSION);
}
