use super::*;

#[test]
fn a_second_sync_waits_then_refuses_while_the_first_holds_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    let first = SyncLock::acquire(&dir).unwrap();

    let started = Instant::now();
    let error = SyncLock::acquire_within(&dir, Duration::from_millis(300))
        .unwrap_err()
        .to_string();

    assert!(error.contains("another vivi sync is running"), "{error}");
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "the second sync waits before it gives up"
    );
    drop(first);
}

#[test]
fn the_lock_is_free_again_once_the_holder_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    drop(SyncLock::acquire(&dir).unwrap());

    SyncLock::acquire_within(&dir, Duration::from_millis(50)).unwrap();
}

#[test]
fn a_waiting_sync_proceeds_when_the_holder_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    let first = SyncLock::acquire(&dir).unwrap();
    let releaser = std::thread::spawn(move || {
        sleep(Duration::from_millis(200));
        drop(first);
    });

    SyncLock::acquire_within(&dir, Duration::from_secs(5)).unwrap();

    releaser.join().unwrap();
}

#[test]
fn the_lock_file_lives_under_the_gitignored_sync_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");

    let _lock = SyncLock::acquire(&dir).unwrap();

    assert!(dir.join(LOCK_FILE).exists());
    assert_eq!(
        std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
        "*\n"
    );
}
