use super::*;

#[test]
fn windows_parse_and_the_default_is_a_day() {
    assert_eq!(Window::parse("1h"), Some(Window::Hour));
    assert_eq!(Window::parse("7d"), Some(Window::Week));
    assert_eq!(Window::parse("24h"), Some(Window::Day));
    assert_eq!(Window::parse("junk"), None);
    assert_eq!(Window::default(), Window::Day);
}

#[test]
fn each_window_names_its_length() {
    let secs: Vec<i64> = Window::ALL.iter().map(|window| window.secs()).collect();
    assert_eq!(secs, [3600, 86_400, 604_800]);
}
