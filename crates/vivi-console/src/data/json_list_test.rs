use super::json_strings;

#[test]
fn a_list_of_strings_is_read() {
    assert_eq!(
        json_strings(r#"["a/b.rs", "c d","path:x/y"]"#),
        ["a/b.rs", "c d", "path:x/y"]
    );
}

#[test]
fn escapes_are_decoded() {
    assert_eq!(
        json_strings(r#"["q\"uote", "back\\slash", "tab\tnl\n", "é"]"#),
        ["q\"uote", "back\\slash", "tab\tnl\n", "\u{e9}"]
    );
}

#[test]
fn an_empty_or_odd_value_is_an_empty_list() {
    assert!(json_strings("[]").is_empty());
    assert!(json_strings("").is_empty());
    assert!(json_strings("{}").is_empty());
    assert!(json_strings("null").is_empty());
}

#[test]
fn a_cut_off_list_keeps_what_was_read() {
    assert_eq!(json_strings(r#"["one", "tw"#), ["one", "tw"]);
}
