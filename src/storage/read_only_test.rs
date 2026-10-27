//! Companion tests for the explicit WAL read policies:
//! [`ReadOnlyWalPolicy`], [`effective_reader_uid`], and
//! [`read_only_wal_index_refused`].

use super::*;

/// Sidecar path of `db_path` (`pump.db` maps to `pump.db-shm`: `-shm` is
/// appended to the full file name, never re-extending it).
fn sidecar(db_path: &Path, suffix: &str) -> PathBuf {
    let mut name = db_path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// A WAL-mode database at `dir/filename` whose `-shm` and `-wal` sidecars
/// have been removed, so the WAL index is genuinely missing. The database
/// was checkpointed before close, so the single file is self-contained.
fn wal_fixture(filename: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(filename);
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE t (x TEXT);
         INSERT INTO t VALUES ('committed row');
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .unwrap();
    drop(conn);
    let _ = fs::remove_file(sidecar(&db, "-shm"));
    let _ = fs::remove_file(sidecar(&db, "-wal"));
    (dir, db)
}

/// A rollback-journal database (header bytes 18/19 not both 2) that never
/// had sidecars.
fn rollback_fixture(filename: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(filename);
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE t (x TEXT);").unwrap();
    drop(conn);
    (dir, db)
}

#[test]
fn reader_uid_gates_allow_database_owner_for_both_database_filenames() {
    for filename in ["mail.sqlite", "pump.db"] {
        let (_dir, db) = wal_fixture(filename);
        let owner = file_owner_uid(&db).unwrap();
        assert_eq!(file_owner_uid(&db), effective_reader_uid());

        // Proven equality with the owner is exactly the grant.
        assert!(!read_only_wal_index_refused(
            &db,
            ReadOnlyWalPolicy::AllowDatabaseOwner,
            Some(owner),
        ));
        // Unequal and unknown reader identities never grant permission.
        assert!(read_only_wal_index_refused(
            &db,
            ReadOnlyWalPolicy::AllowDatabaseOwner,
            Some(owner.wrapping_add(1)),
        ));
        assert!(read_only_wal_index_refused(
            &db,
            ReadOnlyWalPolicy::AllowDatabaseOwner,
            None,
        ));
    }
}

#[test]
fn strict_policy_refuses_even_the_database_owner() {
    for filename in ["mail.sqlite", "pump.db"] {
        let (_dir, db) = wal_fixture(filename);
        let owner = file_owner_uid(&db).unwrap();
        assert!(read_only_wal_index_refused(
            &db,
            ReadOnlyWalPolicy::ExistingOnly,
            Some(owner),
        ));
    }
}

#[test]
fn existing_sidecar_and_non_wal_databases_never_refuse() {
    for filename in ["mail.sqlite", "pump.db"] {
        let (dir, db) = wal_fixture(filename);
        fs::write(sidecar(&db, "-shm"), b"wal-index").unwrap();
        for policy in [
            ReadOnlyWalPolicy::ExistingOnly,
            ReadOnlyWalPolicy::AllowDatabaseOwner,
        ] {
            assert!(
                !read_only_wal_index_refused(&db, policy, None),
                "{filename} with sidecar refused under {policy:?}",
            );
        }
        drop(fs::remove_file(sidecar(&db, "-shm")));
        drop(dir);

        let (_dir, db) = rollback_fixture(filename);
        for policy in [
            ReadOnlyWalPolicy::ExistingOnly,
            ReadOnlyWalPolicy::AllowDatabaseOwner,
        ] {
            assert!(
                !read_only_wal_index_refused(&db, policy, None),
                "non-WAL {filename} refused under {policy:?}",
            );
        }
    }
}

#[test]
fn guard_evaluation_never_creates_sidecars_or_changes_the_database() {
    for filename in ["mail.sqlite", "pump.db"] {
        let (_dir, db) = wal_fixture(filename);
        let before = fs::read(&db).unwrap();
        for policy in [
            ReadOnlyWalPolicy::ExistingOnly,
            ReadOnlyWalPolicy::AllowDatabaseOwner,
        ] {
            for reader_uid in [Some(0_u32), None] {
                let _ = read_only_wal_index_refused(&db, policy, reader_uid);
            }
        }
        assert!(!sidecar(&db, "-shm").exists());
        assert!(!sidecar(&db, "-wal").exists());
        assert_eq!(fs::read(&db).unwrap(), before);
    }
}

#[test]
fn allow_database_owner_opens_a_sidecarless_wal_database_for_its_owner() {
    let (dir, db) = wal_fixture("mail.sqlite");
    assert_eq!(effective_reader_uid(), file_owner_uid(&db));

    // The strict public form still refuses the same database while the WAL
    // index is missing, and creates nothing.
    let error = Storage::open_mailspace_read_only(dir.path())
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("mail.sqlite-shm"), "{error}");
    assert!(!sidecar(&db, "-shm").exists());

    let storage = Storage::open_mailspace_read_only_with_policy(
        dir.path(),
        ReadOnlyWalPolicy::AllowDatabaseOwner,
    );
    assert!(
        storage.is_ok(),
        "{:?}",
        storage.err().map(|e| e.to_string())
    );

    let conn = Storage::open_mailspace_connection_read_only(
        dir.path(),
        ReadOnlyWalPolicy::AllowDatabaseOwner,
    )
    .unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    drop(conn);
    drop(dir);
}

#[test]
fn read_only_wal_index_missing_stays_owner_blind_for_the_doctor() {
    // The Doctor-facing method is bound to the mailspace database filename
    // and answers strictly by existence, never by identity.
    let (dir, db) = wal_fixture("mail.sqlite");
    assert!(Storage::read_only_wal_index_missing(dir.path()));
    assert!(read_only_wal_index_refused(
        &db,
        ReadOnlyWalPolicy::ExistingOnly,
        None,
    ));
    fs::write(sidecar(&db, "-shm"), b"wal-index").unwrap();
    assert!(!Storage::read_only_wal_index_missing(dir.path()));
    drop(fs::remove_file(sidecar(&db, "-shm")));
    drop(dir);

    let (dir, db) = rollback_fixture("mail.sqlite");
    assert!(!Storage::read_only_wal_index_missing(dir.path()));
    drop(fs::remove_file(&db));
    // A missing database is not a WAL database: no refusal, and asking
    // creates nothing.
    assert!(!Storage::read_only_wal_index_missing(dir.path()));
    drop(dir);
}
