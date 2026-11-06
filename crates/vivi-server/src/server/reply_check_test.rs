use super::*;

#[test]
fn a_valid_hand_reply_is_accepted_with_its_answer() {
    let (status, text) = check("hand", "+++\nanswer = \"built\"\n+++\n\nreport\n");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "accepted: answer built\n");
}

#[test]
fn a_markdown_reply_is_rejected_with_the_parser_error() {
    let (status, text) = check("slicer", "## Slices\n\n- one\n");
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(text.starts_with("rejected: "), "{text}");
}

#[test]
fn an_unknown_kind_is_not_found() {
    let (status, _) = check("wizard", "+++\nanswer = \"built\"\n+++\n");
    assert_eq!(status, StatusCode::NOT_FOUND);
}
