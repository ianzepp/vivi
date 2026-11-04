//! Send-side safety rails shared with VC-4: per-identity write rate limit and
//! the message body cap from the contract hardening summary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default write budget: 30 per minute per identity; over → 429
/// `rate_limited`. A deployment overrides it with `SEND_RATE_PER_MINUTE`.
pub const DEFAULT_SEND_RATE_PER_MINUTE: u32 = 30;
/// Contract: 64 KiB UTF-8 body cap; over → 413 `body_too_large`.
pub const BODY_CAP_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq, Eq, Hash)]
struct SeatKey {
    room_id: String,
    seat: u32,
}

#[derive(Clone, Copy)]
struct Window {
    minute: u64,
    count: u32,
}

/// In-memory fixed-window limiter: `limit` writes per 60 s bucket per
/// (project, identity). `None` never refuses. Every write path (rooms, the
/// projects API, MCP) calls `allow_send` before storage.
#[derive(Clone)]
pub struct RateLimiter {
    limit: Option<u32>,
    windows: Arc<Mutex<HashMap<SeatKey, Window>>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(Some(DEFAULT_SEND_RATE_PER_MINUTE))
    }
}

/// Parse `SEND_RATE_PER_MINUTE`: unset is the default, a positive integer
/// is the limit, `0` is unlimited (`None`). Anything else is an error.
pub fn parse_send_rate(raw: Option<&str>) -> Result<Option<u32>, String> {
    let Some(raw) = raw else {
        return Ok(Some(DEFAULT_SEND_RATE_PER_MINUTE));
    };
    match raw.parse::<u32>() {
        Ok(0) => Ok(None),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err(format!(
            "invalid SEND_RATE_PER_MINUTE: {raw:?} (expected a non-negative integer; 0 means unlimited)"
        )),
    }
}

impl RateLimiter {
    /// A limiter allowing `limit` writes per minute per identity; `None`
    /// allows any number.
    pub fn new(limit: Option<u32>) -> Self {
        Self {
            limit,
            windows: Arc::default(),
        }
    }

    /// The configured per-minute limit; `None` is unlimited.
    pub fn limit(&self) -> Option<u32> {
        self.limit
    }

    /// Count one send attempt; true when the seat is under the limit.
    pub fn allow_send(&self, room_id: &str, seat: u32) -> bool {
        let Some(limit) = self.limit else {
            return true;
        };
        let mut guard = self.guard();
        let key = SeatKey {
            room_id: room_id.to_owned(),
            seat,
        };
        let minute = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() / 60)
            .unwrap_or(0);
        let window = guard.entry(key).or_insert(Window { minute, count: 0 });
        if window.minute != minute {
            window.minute = minute;
            window.count = 0;
        }
        window.count += 1;
        window.count <= limit
    }

    fn guard(&self) -> MutexGuard<'_, HashMap<SeatKey, Window>> {
        self.windows.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Whether a send body (UTF-8 bytes) exceeds the contract cap.
pub fn body_over_cap(len: usize) -> bool {
    len > BODY_CAP_BYTES
}
