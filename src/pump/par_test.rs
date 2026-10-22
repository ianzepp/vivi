use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::map_bounded;

/// Block until `n` callers are inside at once; false if they never are
/// (the guard only bounds a failing run).
fn rendezvous(inside: &AtomicUsize, n: usize) -> bool {
    inside.fetch_add(1, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(20);
    while inside.load(Ordering::SeqCst) < n {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::yield_now();
    }
    true
}

#[test]
fn results_come_back_in_item_order() {
    let items: Vec<u32> = (0..9).collect();
    let out = map_bounded(&items, 4, |n| n * 10);
    assert_eq!(
        out,
        items.iter().map(|n| Some(n * 10)).collect::<Vec<_>>(),
        "slot i holds item i's result"
    );
}

#[test]
fn limit_one_runs_items_in_order_on_the_calling_thread() {
    let caller = std::thread::current().id();
    let seen = std::sync::Mutex::new(Vec::new());
    let items = [1, 2, 3];
    map_bounded(&items, 1, |n| {
        assert_eq!(std::thread::current().id(), caller);
        seen.lock().unwrap().push(*n);
    });
    assert_eq!(*seen.lock().unwrap(), [1, 2, 3]);
}

#[test]
fn several_items_are_inside_at_once_up_to_the_limit() {
    let inside = AtomicUsize::new(0);
    let items = [0, 1, 2];
    let out = map_bounded(&items, 3, |_| rendezvous(&inside, 3));
    assert_eq!(out, [Some(true), Some(true), Some(true)]);
}

#[test]
fn the_limit_bounds_how_many_run_at_once() {
    let live = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let items: Vec<u32> = (0..12).collect();
    map_bounded(&items, 3, |_| {
        let now = live.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        std::thread::yield_now();
        live.fetch_sub(1, Ordering::SeqCst);
    });
    assert!(peak.load(Ordering::SeqCst) <= 3, "{peak:?}");
}

#[test]
fn a_panicking_item_loses_only_its_own_slot() {
    let items = [1, 2, 3];
    let out = map_bounded(&items, 2, |n| {
        assert_ne!(*n, 2, "boom");
        *n
    });
    assert_eq!(out, [Some(1), None, Some(3)]);
}
