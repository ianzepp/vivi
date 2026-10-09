//! Send-side safety rails shared with VC-4: per-seat send rate limit and the
//! message body cap from the contract hardening summary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Contract: 30 sends per minute per seat; over → 429 `rate_limited`.
pub const SEND_RATE_PER_MINUTE: u32 = 30;
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

/// In-memory fixed-window limiter: 30 sends per 60 s bucket per (room, seat).
/// VC-4 calls `allow_send` from the send endpoint before storage.
#[derive(Clone, Default)]
pub struct RateLimiter(Arc<Mutex<HashMap<SeatKey, Window>>>);

impl RateLimiter {
    /// Count one send attempt; true when the seat is under the limit.
    pub fn allow_send(&self, room_id: &str, seat: u32) -> bool {
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
        window.count <= SEND_RATE_PER_MINUTE
    }

    fn guard(&self) -> MutexGuard<'_, HashMap<SeatKey, Window>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Whether a send body (UTF-8 bytes) exceeds the contract cap.
pub fn body_over_cap(len: usize) -> bool {
    len > BODY_CAP_BYTES
}
