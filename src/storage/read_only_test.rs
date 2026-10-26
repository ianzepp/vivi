//! Tests for the shared ownership-aware WAL sidecar guard.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{
    WalSidecarPolicy, guard_wal_sidecar, guard_wal_sidecar_with, shm_path, wal_index_missing,
};
use crate::error::VivariumError;

/// Every database filename the shared guard must cover.
const DATABASE_FILENAMES: [&str; 2] = ["mail.sqlite", "pump.db"];

/// A committed value that exists only in the copied WAL, never in the
/// copied database bytes.
const WAL_ONLY_MARKER: &str = "wal-only-probe-value-42";

/// A database file and its committed WAL, copied into an isolated directory
/// without the `-shm` index.
struct WalFixture {
    _dir: tempfile::TempDir,
    db: PathBuf,
    wal: PathBuf,
}

impl WalFixture {
    fn dir(&self) -> &Path {
        self._dir.path()
    }
}

/// Byte snapshot of every file in a fixture directory.
#[derive(Debug, PartialEq, Eq)]
struct DirectoryBytes(BTreeMap<String, Vec<u8>>);

/// Build a real WAL-mode database with one committed value, then copy the
/// quiescent database and its committed WAL into a fresh isolated directory.
/// Never copies the writer's `-shm` index and never unlinks a live writer
/// file.
fn wal_fixture(db_filename: &str) -> WalFixture {
    let source = tempfile::tempdir().unwrap();
    let source_db = source.path().join(db_filename);
    let writer = rusqlite::Connection::open(&source_db).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute_batch("CREATE TABLE probe (value TEXT NOT NULL)")
        .unwrap();
    writer
        .execute("INSERT INTO probe (value) VALUES (?1)", [WAL_ONLY_MARKER])
        .unwrap();
    // The writer is quiescent: the value committed and no transaction is
    // open. Copy the database and its WAL; never the `-shm` index.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_filename);
    let wal = dir.path().join(format!("{db_filename}-wal"));
    fs::copy(&source_db, &db).unwrap();
    fs::copy(source.path().join(format!("{db_filename}-wal")), &wal).unwrap();
    drop(writer);
    let fixture = WalFixture { _dir: dir, db, wal };
    assert_fixture_is_wal_only(&fixture);
    fixture
}

/// Prove the copy is a WAL-mode database with the committed value living
/// only in the WAL, and no `-shm` index.
fn assert_fixture_is_wal_only(fixture: &WalFixture) {
    let db_bytes = fs::read(&fixture.db).unwrap();
    let wal_bytes = fs::read(&fixture.wal).unwrap();
    assert_eq!(
        &db_bytes[18..20],
        &[2, 2],
        "fixture database must be in WAL mode"
    );
    assert!(
        !contains(&db_bytes, WAL_ONLY_MARKER),
        "the committed value must not be checkpointed into the copied database"
    );
    assert!(
        contains(&wal_bytes, WAL_ONLY_MARKER),
        "the committed value must live in the copied WAL"
    );
    assert!(
        !shm_path(&fixture.db).exists(),
        "the fixture must be copied without the -shm index"
    );
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn snapshot(dir: &Path) -> DirectoryBytes {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        assert!(path.is_file(), "unexpected directory entry {path:?}");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        files.insert(name, fs::read(&path).unwrap());
    }
    DirectoryBytes(files)
}

/// Refusals must leave database bytes and sidecars exactly as they were.
fn assert_unchanged(fixture: &WalFixture, before: &DirectoryBytes) {
    assert_eq!(
        snapshot(fixture.dir()),
        *before,
        "database bytes and sidecars must not change"
    );
}

/// The fixture's committed value must be recoverable from the copied pair
/// alone: genuinely in the WAL, not stale bytes.
#[test]
fn fixture_carries_a_committed_wal_only_value() {
    for name in DATABASE_FILENAMES {
        let fixture = wal_fixture(name);
        let scratch = tempfile::tempdir().unwrap();
        fs::copy(&fixture.db, scratch.path().join(name)).unwrap();
        fs::copy(&fixture.wal, scratch.path().join(format!("{name}-wal"))).unwrap();
        let reader = rusqlite::Connection::open(scratch.path().join(name)).unwrap();
        let value: String = reader
            .query_row("SELECT value FROM probe LIMIT 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, WAL_ONLY_MARKER);
    }
}

#[test]
fn foreign_reader_refuses_missing_shm() {
    for name in DATABASE_FILENAMES {
        let fixture = wal_fixture(name);
        let before = snapshot(fixture.dir());
        let error = guard_wal_sidecar_with(
            &fixture.db,
            WalSidecarPolicy::AllowOwnedSidecar,
            |_: &Path| Ok(Some(488_792)),
            || Some(488_793),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains(name), "{message}");
        assert!(message.contains("owned by user"), "{message}");
        assert!(message.contains("refusing"), "{message}");
        assert_unchanged(&fixture, &before);
    }
}

#[test]
fn unknown_owner_refuses_missing_shm() {
    for name in DATABASE_FILENAMES {
        let fixture = wal_fixture(name);
        let before = snapshot(fixture.dir());
        let error = guard_wal_sidecar_with(
            &fixture.db,
            WalSidecarPolicy::AllowOwnedSidecar,
            |_: &Path| Ok(None),
            || Some(488_793),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains(name), "{message}");
        assert!(message.contains("cannot determine the owner"), "{message}");
        assert_unchanged(&fixture, &before);
    }
}

#[test]
fn unknown_effective_identity_refuses_missing_shm() {
    for name in DATABASE_FILENAMES {
        let fixture = wal_fixture(name);
        let before = snapshot(fixture.dir());
        let error = guard_wal_sidecar_with(
            &fixture.db,
            WalSidecarPolicy::AllowOwnedSidecar,
            |_: &Path| Ok(Some(488_792)),
            || None,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("effective user"), "{message}");
        assert_unchanged(&fixture, &before);
    }
}

#[test]
fn strict_guard_refuses_missing_sidecar_even_for_the_owner() {
    for name in DATABASE_FILENAMES {
        let fixture = wal_fixture(name);
        let before = snapshot(fixture.dir());
        let error = guard_wal_sidecar_with(
            &fixture.db,
            WalSidecarPolicy::ExistingSidecarOnly,
            |_: &Path| Ok(Some(488_792)),
            || Some(488_792),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains(name), "{message}");
        assert!(message.contains("refusing to create it"), "{message}");
        assert_unchanged(&fixture, &before);
    }
}

fn never_owner_lookup(_db: &Path) -> io::Result<Option<u32>> {
    panic!("ownership must not be consulted while the sidecar exists")
}

fn never_effective_lookup() -> Option<u32> {
    panic!("effective identity must not be consulted while the sidecar exists")
}

/// With the shared-memory index present the guard allows both policies and
/// must leave the live writer's index alone.
#[test]
fn existing_sidecar_allows_without_identity_checks() {
    for name in DATABASE_FILENAMES {
        let fixture = wal_fixture(name);
        let writer = rusqlite::Connection::open(&fixture.db).unwrap();
        writer
            .execute("INSERT INTO probe (value) VALUES (?1)", ["live-writer"])
            .unwrap();
        assert!(shm_path(&fixture.db).exists());
        let before = snapshot(fixture.dir());
        for policy in [
            WalSidecarPolicy::ExistingSidecarOnly,
            WalSidecarPolicy::AllowOwnedSidecar,
        ] {
            guard_wal_sidecar_with(
                &fixture.db,
                policy,
                never_owner_lookup,
                never_effective_lookup,
            )
            .unwrap();
        }
        assert_unchanged(&fixture, &before);
        drop(writer);
    }
}

/// Operator ruling: the process that owns the database file may take the
/// normal read-only open, and the guard itself still creates nothing.
#[test]
#[cfg(unix)]
fn owned_database_allows_normal_read_only_open() {
    for name in DATABASE_FILENAMES {
        let fixture = wal_fixture(name);
        guard_wal_sidecar(&fixture.db, WalSidecarPolicy::AllowOwnedSidecar).unwrap();
        assert!(
            !shm_path(&fixture.db).exists(),
            "the guard decides; it must not create the sidecar itself"
        );
    }
}

#[test]
fn non_wal_database_reads_without_a_sidecar() {
    for name in DATABASE_FILENAMES {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(name);
        let writer = rusqlite::Connection::open(&db).unwrap();
        writer
            .execute_batch("CREATE TABLE probe (value TEXT NOT NULL)")
            .unwrap();
        drop(writer);
        assert_eq!(
            &fs::read(&db).unwrap()[18..20],
            &[1, 1],
            "fixture must be a rollback-journal database"
        );
        assert!(!wal_index_missing(&db).unwrap());
        guard_wal_sidecar(&db, WalSidecarPolicy::ExistingSidecarOnly).unwrap();
        guard_wal_sidecar_with(
            &db,
            WalSidecarPolicy::AllowOwnedSidecar,
            |_: &Path| Ok(None),
            || None,
        )
        .unwrap();
    }
}

#[test]
fn inspection_is_independent_of_ownership() {
    let fixture = wal_fixture("mail.sqlite");
    assert!(wal_index_missing(&fixture.db).unwrap());
    // With a live writer's sidecar present the index is not missing, and
    // the inspection consults no identity at all.
    let writer = rusqlite::Connection::open(&fixture.db).unwrap();
    writer
        .execute("INSERT INTO probe (value) VALUES (?1)", ["live-writer"])
        .unwrap();
    assert!(!wal_index_missing(&fixture.db).unwrap());
    drop(writer);
}

#[test]
fn filesystem_errors_propagate_from_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("mail.sqlite");
    assert!(matches!(
        wal_index_missing(&missing),
        Err(VivariumError::Io(_))
    ));
    assert!(matches!(
        guard_wal_sidecar(&missing, WalSidecarPolicy::AllowOwnedSidecar),
        Err(VivariumError::Io(_))
    ));

    // A database too short for the SQLite header cannot be inspected.
    let short = dir.path().join("pump.db");
    fs::write(&short, b"SQLi").unwrap();
    assert!(matches!(
        guard_wal_sidecar(&short, WalSidecarPolicy::ExistingSidecarOnly),
        Err(VivariumError::Io(_))
    ));

    // A directory in place of a database is a read error, not a refusal.
    let directory = dir.path().join("else.sqlite");
    fs::create_dir(&directory).unwrap();
    assert!(matches!(
        wal_index_missing(&directory),
        Err(VivariumError::Io(_))
    ));
}
