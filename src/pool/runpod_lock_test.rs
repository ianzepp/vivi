use std::cell::Cell;

use super::*;

#[test]
fn the_lock_is_held_until_dropped_and_a_second_taker_waits_then_gives_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = path_for(dir.path(), "ms", "gpu");
    let told = Cell::new(0);
    let note = |_: &str| told.set(told.get() + 1);
    let first = acquire(&path, Duration::ZERO, Duration::ZERO, &note).unwrap();
    // The margins are generous on purpose: under a full test suite's load a
    // 30ms window can be descheduled away whole, and the protocol (wait,
    // announce once, give up) is what is proven here, not the milliseconds.
    let err = acquire(
        &path,
        Duration::from_millis(500),
        Duration::from_millis(20),
        &note,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("another dispatch"), "{err}");
    assert_eq!(told.get(), 1, "the wait is announced once");
    drop(first);
    assert!(acquire(&path, Duration::ZERO, Duration::ZERO, &note).is_ok());
}

#[test]
fn pools_do_not_share_a_lock() {
    let dir = tempfile::tempdir().unwrap();
    let a = path_for(dir.path(), "ms", "gpu-a");
    let b = path_for(dir.path(), "ms", "gpu-b");
    let _held = acquire(&a, Duration::ZERO, Duration::ZERO, &|_| {}).unwrap();
    assert!(acquire(&b, Duration::ZERO, Duration::ZERO, &|_| {}).is_ok());
}
