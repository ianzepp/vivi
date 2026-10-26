use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::pool::slot::PoolId;

fn id(mailspaces: &str, pool: &str) -> PoolId {
    PoolId::keyed(mailspaces, pool)
}

fn lock_path(dir: &Path, mailspaces: &str, pool: &str) -> PathBuf {
    path_for(dir, &id(mailspaces, pool))
}

#[test]
fn the_lock_is_held_until_dropped_and_contention_never_waits() {
    let dir = tempfile::tempdir().unwrap();
    let path = lock_path(dir.path(), "ms", "gpu");
    let first = try_acquire(&path).unwrap().expect("first taker holds");
    // Contention answers immediately: None, not an error, not a wait.
    assert!(
        try_acquire(&path).unwrap().is_none(),
        "second taker is told no"
    );
    drop(first);
    let again = try_acquire(&path).unwrap();
    assert!(again.is_some(), "the lock is free once the holder drops");
}

#[test]
fn the_lock_is_reacquired_after_an_error_unwinds_past_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = lock_path(dir.path(), "ms", "gpu");
    fn fails_while_holding(path: &Path) -> Result<(), VivariumError> {
        let _lock = try_acquire(path)?.expect("held during the work");
        Err(VivariumError::Message("the work failed".into()))
    }
    assert!(fails_while_holding(&path).is_err(), "the error escapes");
    let again = try_acquire(&path).unwrap();
    assert!(again.is_some(), "unwinding the error released the lock");
}

#[test]
fn paths_differ_by_pool_and_by_mailspace_and_holds_do_not_collide() {
    let dir = tempfile::tempdir().unwrap();
    let a = lock_path(dir.path(), "ms-a", "gpu");
    let b = lock_path(dir.path(), "ms-a", "gpu-b");
    let c = lock_path(dir.path(), "ms-b", "gpu");
    assert_ne!(a, b, "distinct pools of one mailspace differ");
    assert_ne!(a, c, "one pool across mailspace identities differs");
    let held = try_acquire(&a).unwrap().expect("held");
    assert!(try_acquire(&b).unwrap().is_some(), "another pool is free");
    assert!(
        try_acquire(&c).unwrap().is_some(),
        "another mailspace is free"
    );
    assert!(try_acquire(&a).unwrap().is_none());
    drop(held);
}

#[test]
fn the_lock_file_is_stable_and_reused_never_unlinked() {
    let dir = tempfile::tempdir().unwrap();
    let path = lock_path(dir.path(), "ms", "gpu");
    {
        let _held = try_acquire(&path).unwrap().expect("held");
        assert!(path.is_file(), "the lock file exists while held");
    }
    assert!(path.is_file(), "dropping the lock never unlinks the file");
    assert_eq!(fs::metadata(&path).unwrap().len(), 0, "never truncated");
    let reused = try_acquire(&path).unwrap();
    assert!(reused.is_some(), "the stable file takes a lock again");
    assert_eq!(fs::metadata(&path).unwrap().len(), 0, "still unwritten");
}

#[test]
fn a_real_io_error_stays_an_error() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the locks directory must go makes every open fail.
    fs::write(dir.path().join("locks"), b"not a directory").unwrap();
    let err = try_acquire(&lock_path(dir.path(), "ms", "gpu"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot lock"), "{err}");
}
