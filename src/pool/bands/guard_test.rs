//! Companion tests for [`super`]: contention, release, error unwinding,
//! key independence, and exclusion between separately launched processes.

use std::path::Path;
use std::time::{Duration, Instant};

use super::*;

const POOL: &str = "atlas";
/// The full test name, used to relaunch this test as the contending child.
const SELF: &str = "pool::bands::guard::tests::separately_launched_processes_exclude_each_other";
/// Upper bound for any wait; synchronization itself polls every [`TICK`].
const BUDGET: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(5);
const CHILD_ENV: &str = "VIVI_DISPATCH_GUARD_TEST_SCRATCH";

/// How the child half of the cross-process test spends its life: take the
/// lock, tell the parent it is held, and keep holding until told to stop.
fn hold_the_lock_until_released(scratch: &Path) {
    let space = scratch.join("space");
    let guard = DispatchGuard::try_acquire(&space, POOL)
        .expect("child: lock attempt failed")
        .expect("child: lock was free at startup");
    std::fs::write(scratch.join("guard-ready"), b"held").expect("child: ready marker");
    let started = Instant::now();
    while !scratch.join("guard-release").exists() {
        assert!(
            started.elapsed() < BUDGET,
            "child: parent never released the lock"
        );
        std::thread::sleep(TICK);
    }
    drop(guard);
}

#[test]
fn a_second_acquire_of_one_pool_is_immediate_contention() {
    let space = tempfile::tempdir().unwrap();
    let _held = DispatchGuard::try_acquire(space.path(), POOL)
        .unwrap()
        .unwrap();
    let started = Instant::now();
    assert!(
        DispatchGuard::try_acquire(space.path(), POOL)
            .unwrap()
            .is_none(),
        "a held lock must read as contention"
    );
    assert!(
        started.elapsed() < BUDGET,
        "contention must be answered immediately, not waited on"
    );
}

#[test]
fn dropping_the_guard_releases_the_pool() {
    let space = tempfile::tempdir().unwrap();
    let guard = DispatchGuard::try_acquire(space.path(), POOL)
        .unwrap()
        .unwrap();
    assert!(
        DispatchGuard::try_acquire(space.path(), POOL)
            .unwrap()
            .is_none()
    );
    drop(guard);
    assert!(
        DispatchGuard::try_acquire(space.path(), POOL)
            .unwrap()
            .is_some(),
        "the lock must be free again once the holder is dropped"
    );
}

#[test]
fn an_io_error_is_reported_and_leaves_the_guard_usable() {
    let space = tempfile::tempdir().unwrap();
    let blocker = space.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let error = DispatchGuard::try_acquire(&blocker, POOL)
        .expect_err("a file in place of the mailspace directory must fail");
    assert!(
        error.to_string().contains("cannot lock"),
        "unexpected error wording: {error}"
    );
    assert!(
        DispatchGuard::try_acquire(space.path(), POOL)
            .unwrap()
            .is_some(),
        "an unwound error must not wedge the guard"
    );
}

#[test]
fn pools_and_mailspaces_block_only_themselves() {
    let tmp = tempfile::tempdir().unwrap();
    let (alpha, beta) = (tmp.path().join("alpha"), tmp.path().join("beta"));
    let atlas = DispatchGuard::try_acquire(&alpha, "atlas")
        .unwrap()
        .unwrap();
    let beam = DispatchGuard::try_acquire(&alpha, "beam").unwrap().unwrap();
    let twin = DispatchGuard::try_acquire(&beta, "atlas").unwrap().unwrap();
    assert!(
        DispatchGuard::try_acquire(&alpha, "atlas")
            .unwrap()
            .is_none()
    );
    assert!(
        DispatchGuard::try_acquire(&alpha, "beam")
            .unwrap()
            .is_none()
    );
    assert!(
        DispatchGuard::try_acquire(&beta, "atlas")
            .unwrap()
            .is_none()
    );
    assert!(
        DispatchGuard::try_acquire(&beta, "beam").unwrap().is_some(),
        "a fresh pool in a fresh mailspace must never be blocked"
    );
    drop((atlas, beam, twin));
    assert!(
        DispatchGuard::try_acquire(&alpha, "atlas")
            .unwrap()
            .is_some()
    );
    assert!(
        DispatchGuard::try_acquire(&beta, "atlas")
            .unwrap()
            .is_some()
    );
}

#[test]
fn separately_launched_processes_exclude_each_other() {
    if let Ok(scratch) = std::env::var(CHILD_ENV) {
        hold_the_lock_until_released(Path::new(&scratch));
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let space = tmp.path().join("space");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", SELF, "--test-threads=1"])
        .env(CHILD_ENV, tmp.path())
        .spawn()
        .expect("relaunch this test binary as the contending child");
    let started = Instant::now();
    while !tmp.path().join("guard-ready").exists() {
        assert!(
            started.elapsed() < BUDGET && child.try_wait().unwrap().is_none(),
            "the child never took the lock"
        );
        std::thread::sleep(TICK);
    }
    assert!(
        DispatchGuard::try_acquire(&space, POOL).unwrap().is_none(),
        "a lock held by another process must read as contention here too"
    );
    std::fs::write(tmp.path().join("guard-release"), b"done").unwrap();
    let started = Instant::now();
    loop {
        match child.try_wait().expect("child status") {
            Some(status) => {
                assert!(status.success(), "the child failed: {status}");
                break;
            }
            None => {
                assert!(started.elapsed() < BUDGET, "the child never exited");
                std::thread::sleep(TICK);
            }
        }
    }
    assert!(
        DispatchGuard::try_acquire(&space, POOL).unwrap().is_some(),
        "the lock must be free once the other process has exited"
    );
}
