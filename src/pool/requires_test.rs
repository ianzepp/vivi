use super::*;

fn list(items: &[&str]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

#[test]
fn the_list_is_one_command_per_line() {
    assert_eq!(render_list(&list(&["a b", "c"])), "a b\nc\n");
    assert_eq!(render_list(&[]), "");
}

#[test]
fn a_mark_matches_only_the_exact_configured_list() {
    let configured = list(&["command -v tool", "tool --list"]);
    assert!(mark_matches(&configured, &render_list(&configured)));
    assert!(!mark_matches(&configured, "command -v tool\n"));
    assert!(!mark_matches(
        &configured,
        "command -v tool\ntool --other\n"
    ));
    assert!(!mark_matches(&configured, ""));
    assert!(!mark_matches(&list(&["x"]), "x\nextra\n"));
}
