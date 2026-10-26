//! Tests for the classified read-only inspection and open path.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, ErrorCode};

use super::system_effective_uid;
use super::{IndexPolicy, Inspection, InspectionFailure, ReadOnlyRefusal, inspect_with, open_with};
use crate::storage::Storage;

/// Create a WAL-mode mailspace database with no sidecar files, mirroring the
/// storage regression setup. Returns the database path and its bytes.
fn wal_database_without_sidecars(dir: &Path) -> (PathBuf, Vec<u8>) {
    drop(Storage::open_mailspace(dir).unwrap());
    let db = dir.join("mail.sqlite");
    let shm = db.with_extension("sqlite-shm");
    let wal = db.with_extension("sqlite-wal");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(conn);
    drop(fs::remove_file(&shm));
    drop(fs::remove_file(&wal));
    let before = fs::read(&db).unwrap();
    (db, before)
}

fn assert_no_sidecars(db: &Path) {
    assert!(!db.with_extension("sqlite-shm").exists());
    assert!(!db.with_extension("sqlite-wal").exists());
}

fn assert_write_refused(conn: &Connection) {
    let error = conn
        .execute(
            "INSERT INTO storage_metadata (key, value) VALUES ('x', 'y')",
            [],
        )
        .err()
        .unwrap();
    assert!(
        matches!(
            error,
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == ErrorCode::ReadOnly
        ),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn owner_reads_wal_without_index() {
    let temp = tempfile::tempdir().unwrap();
    let (db, before) = wal_database_without_sidecars(temp.path());

    // Production identity: the test process owns the files it created.
    let verdict = super::inspect(&db, IndexPolicy::OwnerMayCreateIndex).unwrap();
    assert_eq!(verdict, Inspection::OwnedSidecarAllowed);

    let conn = super::open(&db, IndexPolicy::OwnerMayCreateIndex).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM storage_metadata", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(rows > 0);
    assert_write_refused(&conn);
    drop(conn);
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn foreign_reader_refuses_missing_index() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempfile::tempdir().unwrap();
    let (db, before) = wal_database_without_sidecars(temp.path());
    let owner_uid = fs::metadata(&db).unwrap().uid();
    let foreign = owner_uid.wrapping_add(1);

    let error = open_with(&db, IndexPolicy::OwnerMayCreateIndex, &move || {
        Some(foreign)
    })
    .err()
    .unwrap();
    assert!(
        matches!(
            error,
            ReadOnlyRefusal::ForeignIndexMissing { owner_uid: seen, .. }
                if seen == owner_uid
        ),
        "{error}"
    );
    assert_no_sidecars(&db);
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn unknown_owner_does_not_authorize_creation() {
    let temp = tempfile::tempdir().unwrap();
    let (db, before) = wal_database_without_sidecars(temp.path());

    let error = open_with(&db, IndexPolicy::OwnerMayCreateIndex, &|| None)
        .err()
        .unwrap();
    assert!(
        matches!(error, ReadOnlyRefusal::UnknownOwnerIndexMissing { .. }),
        "{error}"
    );
    assert_no_sidecars(&db);
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn strict_policy_refuses_even_when_reader_owns_database() {
    let temp = tempfile::tempdir().unwrap();
    let (db, before) = wal_database_without_sidecars(temp.path());

    // The reader's real identity matches the file owner, and the strict
    // policy must refuse without ever consulting identity.
    let error = open_with(&db, IndexPolicy::ExistingIndexOnly, &system_effective_uid)
        .err()
        .unwrap();
    assert!(
        matches!(error, ReadOnlyRefusal::StrictIndexMissing { .. }),
        "{error}"
    );
    assert_no_sidecars(&db);
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn existing_index_needs_no_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let (db, _) = wal_database_without_sidecars(temp.path());

    // A live writer keeps the WAL index in place while the read-only open
    // runs, so no sidecar would be created even in principle.
    let writer = Connection::open(&db).unwrap();
    let touched: i64 = writer
        .query_row("SELECT COUNT(*) FROM storage_metadata", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(touched > 0);
    let shm = db.with_extension("sqlite-shm");
    assert!(shm.exists());

    let verdict = inspect_with(&db, IndexPolicy::ExistingIndexOnly, &|| None).unwrap();
    assert_eq!(verdict, Inspection::NoIndexNeeded);
    let conn = open_with(&db, IndexPolicy::ExistingIndexOnly, &|| None).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM storage_metadata", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(rows > 0);
    drop(conn);
    drop(writer);
}

#[test]
fn non_wal_database_needs_no_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("plain.sqlite");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE t (x INTEGER)").unwrap();
    drop(conn);
    assert_no_sidecars(&db);

    let verdict = inspect_with(&db, IndexPolicy::OwnerMayCreateIndex, &|| None).unwrap();
    assert_eq!(verdict, Inspection::NoIndexNeeded);
    let conn = open_with(&db, IndexPolicy::OwnerMayCreateIndex, &|| None).unwrap();
    conn.query_row("SELECT COUNT(*) FROM t", [], |row| row.get::<_, i64>(0))
        .unwrap();
}

#[test]
fn missing_database_classifies_as_sqlite_open_failure() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("absent.sqlite");

    let error = super::open(&db, IndexPolicy::ExistingIndexOnly)
        .err()
        .unwrap();
    assert!(
        matches!(error, ReadOnlyRefusal::SqliteOpen { .. }),
        "{error}"
    );
    let error = open_with(&db, IndexPolicy::OwnerMayCreateIndex, &|| None)
        .err()
        .unwrap();
    assert!(
        matches!(error, ReadOnlyRefusal::SqliteOpen { .. }),
        "{error}"
    );
    assert!(!db.exists());
    assert_no_sidecars(&db);
}

#[test]
fn non_file_database_path_is_an_inspection_failure() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("mail.sqlite");
    fs::create_dir(&db).unwrap();

    let error = open_with(&db, IndexPolicy::ExistingIndexOnly, &|| None)
        .err()
        .unwrap();
    assert!(
        matches!(
            error,
            ReadOnlyRefusal::Inspection {
                failure: InspectionFailure::NotAFile,
                ..
            }
        ),
        "{error}"
    );
    assert_no_sidecars(&db);
}

#[test]
fn short_database_file_is_a_header_read_failure() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("mail.sqlite");
    fs::write(&db, b"SQLite").unwrap();

    let error = open_with(&db, IndexPolicy::OwnerMayCreateIndex, &|| None)
        .err()
        .unwrap();
    assert!(
        matches!(
            error,
            ReadOnlyRefusal::Inspection {
                failure: InspectionFailure::HeaderRead(_),
                ..
            }
        ),
        "{error}"
    );
    assert_no_sidecars(&db);
    assert_eq!(fs::read(&db).unwrap(), b"SQLite");
}
