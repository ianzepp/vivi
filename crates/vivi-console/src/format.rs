//! Small text helpers for times, durations and ids. Times are UTC.

use chrono::{DateTime, Utc};
use vivi_storage::storage::short_handle_map;

fn at(epoch: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(epoch, 0)
}

/// `HH:MM` when `epoch` is on the same UTC day as `now`, else `MM-DD HH:MM`.
#[must_use]
pub fn time(epoch: i64, now: i64) -> String {
    let (Some(then), Some(today)) = (at(epoch), at(now)) else {
        return String::from("?");
    };
    if then.date_naive() == today.date_naive() {
        then.format("%H:%M").to_string()
    } else {
        then.format("%m-%d %H:%M").to_string()
    }
}

/// The full UTC timestamp, for a tooltip or a `datetime` attribute.
#[must_use]
pub fn iso(epoch: i64) -> String {
    at(epoch).map_or_else(String::new, |when| {
        when.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    })
}

/// `42s`, `18m`, `1h 04m`, `2d 03h`.
#[must_use]
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {:02}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

/// `3m ago`.
#[must_use]
pub fn ago(secs: i64) -> String {
    format!("{} ago", duration(secs))
}

/// The display handle of a task message, as `vivi` shows it: the short
/// handle for a `msg_` id, else the id itself. This is the pure function
/// `Storage::display_handle` applies to an active message, so it needs no
/// store read.
#[must_use]
pub fn task_handle(message_id: &str) -> String {
    short_handle_map(&[message_id.to_string()])
        .remove(message_id)
        .unwrap_or_else(|| message_id.to_string())
}

/// The first eight characters of an id, for compact columns.
#[must_use]
pub fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
#[path = "format_test.rs"]
mod tests;
