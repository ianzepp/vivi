//! Companion tests for `super`: immediate contention, release on drop and
//! on error, independent keys, and exclusion between separate processes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::DispatchGuard;
use super::lock_path;

/// The pool every test locks; the keys under test are the explicit ones.
const POOL: &str = "gpu";
/// The env var that turns a re-launched test binary into the holder child.
const CHILD_ENV: &str = "VIVI_GUARD_TEST_DIR";
/// Bound on every wait in this file: a hang fails the test, it never
/// blocks the suite.
const BOUND: Duration = Duration::from_secs(10);
/// Poll step of the bounded waits: prompt without spinning hot.
const STEP: Duration = Duration::from_millis(5);

/// Wait, bounded, until `path` exists.
fn wait_until(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < BOUND,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(STEP);
    }
}

/// Touch `path` as a one-way signal between the test processes.
fn signal(path: &Path) {
    std::fs::write(path, b"1").unwrap();
}

#[test]
fn a_second_taker_is_refused_at_once_and_drop_releases_without_unlinking() {
    let dir = tempfile::tempdir().unwrap();
    let path = lock_path(dir.path(), POOL);
    let first = DispatchGuard::try_acquire(dir.path(), POOL).unwrap();
    assert!(first.is_some(), "a free pool must be acquirable");
    let started = Instant::now();
    let second = DispatchGuard::try_acquire(dir.path(), POOL).unwrap();
    assert!(
        second.is_none(),
        "contention must answer None, not block or error"
    );
    assert!(
        started.elapsed() < BOUND,
        "the contention answer must be immediate"
    );
    drop(first);
    let third = DispatchGuard::try_acquire(dir.path(), POOL).unwrap();
    assert!(third.is_some(), "dropping the holder must release the pool");
    drop(third);
    assert!(path.is_file(), "the lock file is never unlinked on release");
}

#[test]
fn an_error_unwinds_cleanly_and_the_guard_still_works_afterward() {
    let dir = tempfile::tempdir().unwrap();
    // A regular file where the mailspace directory belongs makes the lock
    // directory step fail with a real I/O error.
    let blocker = dir.path().join("not-a-mailspace");
    std::fs::write(&blocker, b"x").unwrap();
    let err = DispatchGuard::try_acquire(&blocker, POOL)
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot lock"), "{err}");
    // And a plain file where the locks directory belongs fails the open.
    let dir2 = tempfile::tempdir().unwrap();
    std::fs::write(dir2.path().join("locks"), b"x").unwrap();
    assert!(DispatchGuard::try_acquire(dir2.path(), POOL).is_err());
    // Unwinding both errors poisons nothing: a sound directory acquires.
    let held = DispatchGuard::try_acquire(dir.path(), POOL).unwrap();
    assert!(held.is_some(), "the guard must stay usable after errors");
}

#[test]
fn pools_and_mailspaces_hold_their_guards_independently() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let _a = DispatchGuard::try_acquire(dir.path(), "gpu-a").unwrap();
    let _b = DispatchGuard::try_acquire(dir.path(), "gpu-b").unwrap();
    let _c = DispatchGuard::try_acquire(other.path(), "gpu-a").unwrap();
    // Every distinct key holds its own file; equal pool names across
    // mailspaces share a name, never a file.
    let a = lock_path(dir.path(), "gpu-a");
    let b = lock_path(dir.path(), "gpu-b");
    let c = lock_path(other.path(), "gpu-a");
    assert_ne!(a, b, "distinct pools must hash to distinct files");
    assert_ne!(a, c, "distinct mailspaces must own distinct files");
    assert_eq!(
        a.file_name(),
        c.file_name(),
        "the pool identity is stable across mailspaces"
    );
}

/// The holder child of the cross-process test, run by the parent through a
/// fresh copy of this test binary. Under an ordinary `cargo test` run the
/// env var is absent and this does nothing.
#[test]
fn child_holds_the_pool_guard_until_released() {
    let Some(dir) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    let guard = DispatchGuard::try_acquire(&dir, POOL)
        .unwrap()
        .expect("the child must get the uncontended guard");
    signal(&dir.join("held"));
    wait_until(&dir.join("release"));
    drop(guard);
    signal(&dir.join("dropped"));
}

#[test]
fn a_separate_process_is_refused_then_readmitted_once_it_exits() {
    let dir = tempfile::tempdir().unwrap();
    let output = {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pool::bands::guard::tests::child_holds_the_pool_guard_until_released",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, dir.path())
            .spawn()
            .unwrap();
        // Bounded handshake, no long sleep: the child says when it holds.
        wait_until(&dir.path().join("held"));
        let started = Instant::now();
        let refused = DispatchGuard::try_acquire(dir.path(), POOL).unwrap();
        assert!(
            refused.is_none(),
            "another process holding the guard must refuse us"
        );
        assert!(started.elapsed() < BOUND, "the refusal must be immediate");
        signal(&dir.path().join("release"));
        child.wait_with_output().unwrap()
    };
    assert!(
        output.status.success(),
        "the holder child failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let readmitted = DispatchGuard::try_acquire(dir.path(), POOL).unwrap();
    assert!(
        readmitted.is_some(),
        "the guard must be free once the holder process has exited"
    );
}
