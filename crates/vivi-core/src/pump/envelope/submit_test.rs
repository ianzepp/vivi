use super::*;

#[test]
fn the_section_names_the_url_and_the_heredoc() {
    let text = section("http://h:1/v1/reply/check/hand");
    assert!(text.contains("vivi-submit http://h:1/v1/reply/check/hand <<'VIVI_REPLY'"));
    assert!(text.contains("\nVIVI_REPLY\n"));
    assert!(text.contains("rejected: <reason>"));
}
