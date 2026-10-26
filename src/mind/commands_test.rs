//! What `vivi mind status` prints.

use chrono::{FixedOffset, TimeZone};

use super::status_lines;
use crate::mind::socket::StatusView;
use crate::mind::wake::{WakeRecord, readable_time_in};

fn view(last_wake: Option<WakeRecord>) -> StatusView {
    StatusView {
        authority: "observe".into(),
        paused: false,
        busy: false,
        session_id: None,
        last_wake,
        ignored: 0,
    }
}

#[test]
fn a_time_reads_as_local_date_time_and_offset() {
    let eastern = FixedOffset::west_opt(4 * 3600).unwrap();
    assert_eq!(
        readable_time_in(1_793_015_770, &eastern),
        "2026-10-26 07:56:10 -04:00"
    );
    assert_eq!(
        readable_time_in(1_793_015_770, &chrono::Utc),
        "2026-10-26 11:56:10 +00:00"
    );
    assert!(
        chrono::Utc
            .timestamp_opt(1_793_015_770, 0)
            .single()
            .is_some()
    );
}

#[test]
fn an_unrepresentable_time_prints_as_the_number() {
    assert_eq!(
        readable_time_in(u64::MAX, &chrono::Utc),
        u64::MAX.to_string()
    );
}

#[test]
fn status_prints_wake_times_and_never_bare_unix_seconds() {
    let wake = WakeRecord {
        started_at: 1_793_015_770,
        ended_at: Some(1_793_015_773),
        outcome: Some("success".into()),
        ..WakeRecord::default()
    };
    let lines = status_lines(&view(Some(wake)), true);
    let last = lines.last().unwrap();
    assert!(last.starts_with("last wake started 2026-10-"), "{last}");
    assert!(last.ends_with("outcome success"), "{last}");
    assert!(!last.contains("1793015770"), "{last}");
}

#[test]
fn an_unfinished_wake_says_in_flight() {
    let wake = WakeRecord {
        started_at: 1_793_015_770,
        ..WakeRecord::default()
    };
    let lines = status_lines(&view(Some(wake)), true);
    assert!(
        lines
            .last()
            .unwrap()
            .contains("ended in-flight outcome none")
    );
    assert_eq!(
        status_lines(&view(None), false).last().unwrap(),
        "last wake none"
    );
}
