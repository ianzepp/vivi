//! Bounded fan-out for the pump's remote I/O.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Run `work` over every item with at most `limit` calls in flight, and
/// return the results in item order. A call that panics yields `None` in its
/// slot, so one bad worker never loses the others' results. `limit` 1 (or a
/// single item) runs the items one after another on the calling thread, in
/// order.
pub(crate) fn map_bounded<T, R>(
    items: &[T],
    limit: usize,
    work: impl Fn(&T) -> R + Sync,
) -> Vec<Option<R>>
where
    T: Sync,
    R: Send,
{
    let run = |item: &T| catch_unwind(AssertUnwindSafe(|| work(item))).ok();
    let workers = limit.max(1).min(items.len());
    if workers <= 1 {
        return items.iter().map(run).collect();
    }
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<R>>> = Mutex::new(items.iter().map(|_| None).collect());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = run(item);
                    let mut held = slots
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    held[index] = result;
                }
            });
        }
    });
    slots
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[path = "par_test.rs"]
mod tests;
