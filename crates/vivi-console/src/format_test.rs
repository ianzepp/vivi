use super::*;

#[test]
fn durations_read_naturally() {
    assert_eq!(duration(-5), "0s");
    assert_eq!(duration(42), "42s");
    assert_eq!(duration(18 * 60 + 5), "18m");
    assert_eq!(duration(3600 + 4 * 60), "1h 04m");
    assert_eq!(duration(2 * 86_400 + 3 * 3600), "2d 03h");
    assert_eq!(ago(120), "2m ago");
}

#[test]
fn times_drop_the_date_on_the_same_utc_day() {
    // 2026-11-14T06:29:00Z and 2026-11-14T12:00:00Z
    assert_eq!(time(1_794_637_740, 1_794_657_600), "06:29");
    // the day before
    assert_eq!(time(1_794_637_740 - 86_400, 1_794_657_600), "11-13 06:29");
    assert_eq!(iso(1_794_637_740), "2026-11-14T06:29:00Z");
}

#[test]
fn task_handles_match_the_storage_short_form() {
    assert_eq!(task_handle("msg_0123456789abcdef"), "01234567");
    assert_eq!(task_handle("plain"), "plain");
    assert_eq!(short("0123456789"), "01234567");
}
