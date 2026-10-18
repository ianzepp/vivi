//! Minimal JWT `exp` read. No signature check: the token is ours on disk.

use crate::b64;

/// Unix seconds from a JWT `exp` claim. `None` if the token is not a JWT.
#[must_use]
pub fn exp_unix(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let decoded = decode_b64url(payload)?;
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&decoded) else {
        return None;
    };
    value.get("exp")?.as_i64()
}

fn decode_b64url(input: &str) -> Option<Vec<u8>> {
    let mut std = input.replace('-', "+").replace('_', "/");
    while !std.len().is_multiple_of(4) {
        std.push('=');
    }
    b64::decode(&std)
}

/// True when `exp` is missing or within `skew` seconds of now.
#[must_use]
pub fn expires_within(token: &str, skew_secs: i64, now: i64) -> bool {
    match exp_unix(token) {
        Some(exp) => exp - now <= skew_secs,
        None => true,
    }
}
