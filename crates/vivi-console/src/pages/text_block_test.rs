use super::*;

#[test]
fn values_of_secret_names_are_hidden() {
    for (line, hidden) in [
        ("export API_TOKEN=abc123", "abc123"),
        ("password: hunter2", "hunter2"),
        ("Authorization: Bearer abcdef", "abcdef"),
        ("\"token\": \"zzz999\",", "zzz999"),
        ("curl -H secret= xyz", "xyz"),
    ] {
        let shown = redact(line);
        assert!(!shown.contains(hidden), "{line} -> {shown}");
        assert!(shown.contains(HIDDEN), "{line} -> {shown}");
    }
}

#[test]
fn credential_shaped_words_are_hidden() {
    for word in [
        "ghp_0123456789abcdefghij",
        "sk-abcdefghijklmnop",
        "vivi_0123456789abcdef",
        "github_pat_11AAAA",
        "eyJhbGciOiJIUzI1NiIsInR5",
    ] {
        let shown = redact(&format!("found {word} in the log"));
        assert_eq!(shown, format!("found {HIDDEN} in the log"), "{word}");
    }
}

#[test]
fn ordinary_text_and_layout_survive() {
    let text = "test result: ok. 412 passed\n    indented line\n\nlast";
    assert_eq!(redact(text), text);
    assert_eq!(redact("short sk-1 is not a key"), "short sk-1 is not a key");
}

#[test]
fn a_long_text_is_cut_at_a_character_boundary_keeping_one_end() {
    let text = "é".repeat(10);
    let head = cap(&text, 7, Keep::Head);
    assert!(head.starts_with("ééé\n[cut"), "{head}");
    let tail = cap(&text, 7, Keep::Tail);
    assert!(tail.starts_with("[cut: the start"), "{tail}");
    assert!(tail.ends_with("ééé"), "{tail}");
    assert_eq!(cap("short", 100, Keep::Head), "short");
}

#[test]
fn a_block_is_escaped_in_a_pre() {
    let html = pre("<script>x()</script> & more", Keep::Head).to_string();
    assert!(html.starts_with("<pre class=\"block\">"));
    assert!(html.contains("&lt;script&gt;x()&lt;/script&gt; &amp; more"));
    assert!(!html.contains("<script>"));
}
