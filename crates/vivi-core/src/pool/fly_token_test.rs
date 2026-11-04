use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

static FETCHES: AtomicUsize = AtomicUsize::new(0);

#[allow(clippy::unnecessary_wraps)]
fn counting() -> Result<Secret, VivariumError> {
    let n = FETCHES.fetch_add(1, Ordering::SeqCst);
    Ok(Secret::new(format!("tok{n}")))
}

fn failing() -> Result<Secret, VivariumError> {
    Err(VivariumError::Message("no token".into()))
}

#[test]
fn a_fresh_token_is_reused_and_an_old_one_is_fetched_again() {
    let cache = TokenCache::new(Duration::from_mins(1), counting);
    let first = cache.get().unwrap();
    assert_eq!(cache.get().unwrap().expose(), first.expose());

    let stale = TokenCache::new(Duration::ZERO, counting);
    let a = stale.get().unwrap();
    let b = stale.get().unwrap();
    assert_ne!(a.expose(), b.expose(), "an expired token is replaced");
}

#[test]
fn invalidate_forces_a_new_fetch() {
    let cache = TokenCache::new(Duration::from_mins(1), counting);
    let first = cache.get().unwrap();
    cache.invalidate();
    assert_ne!(cache.get().unwrap().expose(), first.expose());
}

#[test]
fn a_fetch_failure_is_an_error_not_a_stale_token() {
    let cache = TokenCache::new(Duration::ZERO, failing);
    assert!(cache.get().is_err());
}
