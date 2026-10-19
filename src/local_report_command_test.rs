use super::parse_bound;

#[test]
fn date_only_bounds_use_the_same_whole_utc_calendar_day_everywhere() {
    let since = parse_bound(Some("2026-03-08"), false).unwrap().unwrap();
    let until = parse_bound(Some("2026-03-08"), true).unwrap().unwrap();
    assert_eq!(since, 1_772_928_000);
    assert_eq!(until, since + 86_399);
}
