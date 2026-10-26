//! Duplicate-descriptor regressions for the pump lock owner.

use super::*;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// Open (creating) the lock file at `path` the way owner and contender both do.
fn open_lock_file(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap()
}

#[test]
fn dropping_the_owner_unlocks_with_an_open_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("pump.lock");
    let file = open_lock_file(&path);
    // Kept alive through the post-drop assertions: the owner must unlock
    // explicitly, because a plain close would not free the lock while this
    // duplicate of the same open file description is still open.
    let _duplicate = file.try_clone().unwrap();
    let owner = PumpLock::try_lock(file).unwrap();

    let contender = open_lock_file(&path);
    assert!(
        matches!(contender.try_lock(), Err(TryLockError::WouldBlock)),
        "the owner excludes an independently opened contender"
    );

    drop(owner);

    contender
        .try_lock()
        .expect("acquires immediately after the owner drops, while the duplicate is still open");
}

#[test]
fn an_error_path_unlocks_with_an_open_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("pump.lock");
    let file = open_lock_file(&path);
    let _duplicate = file.try_clone().unwrap();

    let outcome = fail_while_holding(PumpLock::try_lock(file).unwrap(), &path);
    assert_eq!(outcome.unwrap_err(), "migration failed");

    let contender = open_lock_file(&path);
    contender
        .try_lock()
        .expect("acquires immediately after the error path, duplicate still open");
}

/// A stand-in for a migration that holds the pump lock and then fails.
fn fail_while_holding(lock: PumpLock, path: &Path) -> Result<(), String> {
    let contender = open_lock_file(path);
    assert!(
        matches!(contender.try_lock(), Err(TryLockError::WouldBlock)),
        "the lock is held while the operation runs"
    );
    let _lock = lock;
    Err(String::from("migration failed"))
}

#[test]
fn a_failed_contender_does_not_unlock_the_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("pump.lock");
    let owner = PumpLock::try_lock(open_lock_file(&path)).unwrap();

    let contender = open_lock_file(&path);
    assert!(matches!(
        contender.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    drop(contender);

    let latecomer = open_lock_file(&path);
    assert!(
        matches!(latecomer.try_lock(), Err(TryLockError::WouldBlock)),
        "the failed contender left the original owner exclusive"
    );

    drop(owner);
    latecomer
        .try_lock()
        .expect("the lock frees when the owner is dropped");
}
