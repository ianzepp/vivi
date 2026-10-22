//! The per-identity write limit: the 30/min default, a configured limit, and
//! `SEND_RATE_PER_MINUTE=0` for no limit.

use vivi_server::safety::{DEFAULT_SEND_RATE_PER_MINUTE, RateLimiter, parse_send_rate};

fn refused_at(limiter: &RateLimiter, attempts: u32) -> Option<u32> {
    (1..=attempts).find(|_| !limiter.allow_send("project", 7))
}

#[test]
fn the_default_refuses_the_thirty_first_write() {
    let limiter = RateLimiter::default();
    assert_eq!(limiter.limit(), Some(30));
    assert_eq!(DEFAULT_SEND_RATE_PER_MINUTE, 30);
    assert_eq!(refused_at(&limiter, 100), Some(31));
}

#[test]
fn a_configured_limit_refuses_the_next_write() {
    let limiter = RateLimiter::new(Some(5));
    assert_eq!(limiter.limit(), Some(5));
    assert_eq!(refused_at(&limiter, 100), Some(6));
}

#[test]
fn the_limit_is_per_identity() {
    let limiter = RateLimiter::new(Some(2));
    assert!(limiter.allow_send("project", 1));
    assert!(limiter.allow_send("project", 1));
    assert!(!limiter.allow_send("project", 1));
    assert!(limiter.allow_send("project", 2));
}

#[test]
fn zero_means_unlimited() {
    let limiter = RateLimiter::new(None);
    assert_eq!(limiter.limit(), None);
    assert_eq!(refused_at(&limiter, 200), None);
}

#[test]
fn the_env_value_parses_to_a_limit() {
    assert_eq!(parse_send_rate(None), Ok(Some(30)));
    assert_eq!(parse_send_rate(Some("5")), Ok(Some(5)));
    assert_eq!(parse_send_rate(Some("1000")), Ok(Some(1000)));
    assert_eq!(parse_send_rate(Some("0")), Ok(None));
}

#[test]
fn an_invalid_env_value_is_rejected_by_name() {
    for bad in ["-1", "abc", "", " 5", "5.5", "99999999999"] {
        let err = parse_send_rate(Some(bad)).unwrap_err();
        assert!(err.contains("SEND_RATE_PER_MINUTE"), "{bad:?}: {err}");
    }
}
