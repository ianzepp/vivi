//! Companion tests for the ownership-aware WAL read-only guard.
//!
//! Fixtures copy a live WAL database plus its committed WAL into an
//! isolated directory without the shared-memory sidecar; the source
//! connection closes only after the copy and no live sidecar is ever
//! unlinked. Identity comes from the private injection seam, never from
//! the environment, root, chown, or process-wide uid changes.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::{Identity, WalIndexPolicy, guard_as, open, open_as, wal_index_missing};

const OWNER: u32 = 4242;
const FOREIGN_READER: u32 = 4243;

fn owner_identity() -> Identity {
    Identity {
        owner: |_| Some(OWNER),
        reader: || Some(OWNER),
    }
}

/// Sibling path of a database sidecar (`-wal`, `-shm`), matching the
/// production sidecar naming for any database filename.
fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let name = db
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    db.with_file_name(format!("{name}{suffix}"))
}

/// Copy a WAL database with its committed row into an isolated directory
/// without the `-shm` sidecar, keeping the row reachable only via WAL.
fn wal_only_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().unwrap();
    let source_db = source.path().join(name);
    let conn = Connection::open(&source_db).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch("CREATE TABLE demo(note TEXT NOT NULL);")
        .unwrap();
    // Land the schema in the main database; the next write stays in the WAL
    // (autocommitted, far below the autocheckpoint threshold).
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    conn.execute_batch("INSERT INTO demo(note) VALUES ('from-the-wal');")
        .unwrap();
    let dest = tempfile::tempdir().unwrap();
    let dest_db = dest.path().join(name);
    fs::copy(&source_db, &dest_db).unwrap();
    fs::copy(sidecar(&source_db, "-wal"), sidecar(&dest_db, "-wal")).unwrap();
    drop(conn);
    (dest, dest_db)
}

/// Prove the fixture row lives only in the copied WAL.
fn assert_wal_only(db: &Path) {
    let needle = b"from-the-wal";
    let db_bytes = fs::read(db).unwrap();
    let wal_bytes = fs::read(sidecar(db, "-wal")).unwrap();
    assert!(
        contains_bytes(&wal_bytes, needle),
        "fixture row must live in the copied WAL"
    );
    assert!(
        !contains_bytes(&db_bytes, needle),
        "fixture row must not be checkpointed into the database"
    );
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(unix)]
fn assert_sidecar_owned_by_reader(db: &Path, shm: &Path) {
    use std::os::unix::fs::MetadataExt;

    use super::{file_owner_uid, system_effective_uid};

    let shm_uid = fs::metadata(shm).unwrap().uid();
    assert_eq!(
        Some(shm_uid),
        file_owner_uid(db),
        "sidecar and database share one owner"
    );
    assert_eq!(
        Some(shm_uid),
        system_effective_uid(),
        "sidecar owned by the effective reader"
    );
}

#[test]
fn owner_policy_opens_wal_only_data_and_rejects_writes() {
    let (_dir, db) = wal_only_fixture("pump.db");
    assert_wal_only(&db);
    let before = fs::read(&db).unwrap();

    let conn = open_as(&db, WalIndexPolicy::OwnerMayCreateIndex, owner_identity()).unwrap();
    let note: String = conn
        .query_row("SELECT note FROM demo LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(note, "from-the-wal", "committed WAL data must be visible");

    let error = conn
        .execute_batch("INSERT INTO demo(note) VALUES ('rejected');")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("readonly"), "{error}");
    assert_eq!(
        fs::read(&db).unwrap(),
        before,
        "reads must not rewrite the database"
    );

    let shm = sidecar(&db, "-shm");
    assert!(shm.exists(), "SQLite initialized the sidecar it may create");
    #[cfg(unix)]
    assert_sidecar_owned_by_reader(&db, &shm);
    drop(conn);
}

#[test]
#[cfg(unix)]
fn real_owner_policy_authorizes_the_database_owner() {
    let (_dir, db) = wal_only_fixture("mail.sqlite");
    let conn = open(&db, WalIndexPolicy::OwnerMayCreateIndex).unwrap();
    let note: String = conn
        .query_row("SELECT note FROM demo LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(note, "from-the-wal");
    let shm = sidecar(&db, "-shm");
    assert!(shm.exists());
    assert_sidecar_owned_by_reader(&db, &shm);
    drop(conn);
}

#[test]
#[cfg(not(unix))]
fn real_owner_policy_refuses_without_identity_support() {
    let (_dir, db) = wal_only_fixture("pump.db");
    assert!(open(&db, WalIndexPolicy::OwnerMayCreateIndex).is_err());
    assert!(!sidecar(&db, "-shm").exists());
}

#[test]
fn foreign_reader_is_refused_without_sidecar_or_database_changes() {
    let (_dir, db) = wal_only_fixture("mail.sqlite");
    let before = fs::read(&db).unwrap();
    let foreign = Identity {
        owner: |_| Some(OWNER),
        reader: || Some(FOREIGN_READER),
    };

    let guard_error = guard_as(&db, WalIndexPolicy::OwnerMayCreateIndex, foreign)
        .err()
        .unwrap()
        .to_string();
    assert!(guard_error.contains("mail.sqlite-shm"), "{guard_error}");
    let open_error = open_as(&db, WalIndexPolicy::OwnerMayCreateIndex, foreign)
        .err()
        .unwrap()
        .to_string();
    assert!(open_error.contains("mail.sqlite-shm"), "{open_error}");

    assert!(
        !sidecar(&db, "-shm").exists(),
        "refusal must not create the sidecar"
    );
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn unknown_owner_is_refused_without_creating_the_sidecar() {
    let (_dir, db) = wal_only_fixture("pump.db");
    let before = fs::read(&db).unwrap();
    let unknown_owner = Identity {
        owner: |_| None,
        reader: || Some(FOREIGN_READER),
    };

    assert!(guard_as(&db, WalIndexPolicy::OwnerMayCreateIndex, unknown_owner).is_err());
    assert!(open_as(&db, WalIndexPolicy::OwnerMayCreateIndex, unknown_owner).is_err());
    assert!(!sidecar(&db, "-shm").exists());
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn unknown_reader_is_refused_without_creating_the_sidecar() {
    let (_dir, db) = wal_only_fixture("pump.db");
    let before = fs::read(&db).unwrap();
    let unknown_reader = Identity {
        owner: |_| Some(OWNER),
        reader: || None,
    };

    assert!(guard_as(&db, WalIndexPolicy::OwnerMayCreateIndex, unknown_reader).is_err());
    assert!(open_as(&db, WalIndexPolicy::OwnerMayCreateIndex, unknown_reader).is_err());
    assert!(!sidecar(&db, "-shm").exists());
    assert_eq!(fs::read(&db).unwrap(), before);
}

#[test]
fn strict_policy_still_refuses_a_missing_sidecar() {
    let (_dir, db) = wal_only_fixture("mail.sqlite");
    let before = fs::read(&db).unwrap();
    let wal = sidecar(&db, "-wal");
    let wal_before = fs::read(&wal).unwrap();

    let error = open(&db, WalIndexPolicy::ExistingIndex)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("mail.sqlite-shm"), "{error}");
    assert!(
        !sidecar(&db, "-shm").exists(),
        "strict refusal must not create the sidecar"
    );
    assert_eq!(fs::read(&db).unwrap(), before);
    assert_eq!(fs::read(&wal).unwrap(), wal_before);
}

#[test]
fn wal_index_missing_tracks_header_and_sidecar_for_any_filename() {
    let (_dir, db) = wal_only_fixture("pump.db");
    assert!(wal_index_missing(&db), "WAL header with absent sidecar");

    // A present sidecar clears the flag. The test writes this fixture file
    // itself; the guard never creates one.
    fs::write(sidecar(&db, "-shm"), b"").unwrap();
    assert!(!wal_index_missing(&db));
}

#[test]
fn non_wal_database_opens_read_only_without_a_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("plain.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE demo(note TEXT); INSERT INTO demo VALUES ('x');")
        .unwrap();
    drop(conn);

    assert!(!wal_index_missing(&db));
    let conn = open(&db, WalIndexPolicy::ExistingIndex).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM demo", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    drop(conn);
}
