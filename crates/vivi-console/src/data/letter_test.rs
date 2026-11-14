use super::*;

#[test]
fn a_plain_letter_reads_its_headers_and_body() {
    let letter = parse(b"From: a@x\r\nX-Vivi-Kind: need\r\n\r\nline one\r\nline two\r\n");
    assert_eq!(letter.header("x-vivi-kind"), Some("need"));
    assert_eq!(letter.body, "line one\nline two\n");
    assert!(!letter.is_event());
}

#[test]
fn folded_headers_and_event_letters_are_read() {
    let letter = parse(b"Subject: one\n two\nX-Vivi-Event: done\n\nbody");
    assert_eq!(letter.header("Subject"), Some("one two"));
    assert!(letter.is_event());
}

#[test]
fn quoted_printable_is_decoded_with_soft_breaks() {
    let letter = parse(
        b"Content-Transfer-Encoding: quoted-printable\n\ncaf=C3=A9 and a lo=\nng line\nsecond",
    );
    assert_eq!(letter.body, "caf\u{e9} and a long line\nsecond");
}

#[test]
fn base64_is_decoded() {
    // "hello\nworld" in base64.
    let letter = parse(b"Content-Transfer-Encoding: base64\n\naGVsbG8Kd29y\nbGQ=\n");
    assert_eq!(letter.body, "hello\nworld");
}

#[test]
fn the_first_text_part_of_a_multipart_letter_is_the_body() {
    let raw = b"Content-Type: multipart/alternative; boundary=\"b1\"\n\n\
--b1\nContent-Type: text/plain; charset=utf-8\n\nthe text\n\
--b1\nContent-Type: text/html\n\n<p>the html</p>\n--b1--\n";
    assert_eq!(parse(raw).body, "the text");
}

const NEED: &str = "+++\ndone_when = [\"a is true\", \"b passes\"]\ntarget = \"repo\"\nwrite_scope = [\"x/\"]\n+++\n\nThe prose.";

#[test]
fn a_need_body_gives_done_when_target_and_prose() {
    let text = need_text(NEED);
    assert_eq!(text.facts.done_when, ["a is true", "b passes"]);
    assert_eq!(text.facts.target.as_deref(), Some("repo"));
    assert_eq!(text.prose, "The prose.");
    assert_eq!(text.front_error, None);
}

#[test]
fn an_old_format_need_is_all_prose() {
    let text = need_text("Just words.\n");
    assert!(text.facts.done_when.is_empty());
    assert_eq!(text.prose, "Just words.");
    assert_eq!(text.front_error, None);
}

#[test]
fn unreadable_front_matter_is_named_and_the_prose_kept() {
    let text = need_text("+++\ndone_when = oops\n+++\nthe prose");
    assert!(text.front_error.is_some());
    assert_eq!(text.prose, "the prose");
    assert!(text.facts.done_when.is_empty());
}
