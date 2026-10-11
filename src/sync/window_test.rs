use chrono::TimeZone;

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 11, 12, 30, 0).unwrap()
}

#[test]
fn ages_count_back_from_now() {
    assert_eq!(parse_since("7d", now()).unwrap(), "2026-10-04T12:30:00Z");
    assert_eq!(parse_since("12h", now()).unwrap(), "2026-10-11T00:30:00Z");
    assert_eq!(parse_since("90m", now()).unwrap(), "2026-10-11T11:00:00Z");
}

#[test]
fn rfc3339_times_and_dates_are_normalized_to_utc() {
    assert_eq!(
        parse_since("2026-10-01T08:00:00+02:00", now()).unwrap(),
        "2026-10-01T06:00:00Z"
    );
    assert_eq!(
        parse_since("2026-10-01", now()).unwrap(),
        "2026-10-01T00:00:00Z"
    );
}

#[test]
fn anything_else_names_the_accepted_forms() {
    for bad in ["", "soon", "0d", "2026-13-40", "7 days"] {
        let error = parse_since(bad, now()).unwrap_err().to_string();
        assert!(error.contains("--since"), "{bad}: {error}");
        assert!(error.contains("YYYY-MM-DD"), "{bad}: {error}");
    }
}
