use super::*;

#[test]
fn parses_units_and_bare_seconds() {
    assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
    assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
    assert_eq!(parse_duration("5m").unwrap(), Duration::from_mins(5));
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_hours(1));
    assert_eq!(
        parse_duration("90d").unwrap(),
        Duration::from_secs(90 * 86_400)
    );
    assert_eq!(parse_duration(" 2h ").unwrap(), Duration::from_hours(2));
}

#[test]
fn rejects_zero_and_unknown() {
    assert!(parse_duration("0s").is_err());
    assert!(parse_duration("soon").is_err());
    assert!(parse_duration("5ms").is_err());
    assert!(parse_duration("2hr").is_err());
}
