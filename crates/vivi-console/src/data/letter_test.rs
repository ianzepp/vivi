use super::*;

#[test]
fn a_plain_letter_gives_its_headers_and_body() {
    let raw = b"From: A <a@x>\nDate: Tue, 01 Jan 2030 00:00:00 +0000\nX-Vivi-Kind: Need\nX-Vivi-Event: Done\nSubject: s\n\nhello\n\n- a list\n";
    let letter = parse(raw);
    assert_eq!(letter.kind.as_deref(), Some("need"));
    assert_eq!(letter.event.as_deref(), Some("done"));
    assert!(letter.is_event());
    assert_eq!(letter.at, Some(1_893_456_000));
    assert_eq!(letter.from, "A <a@x>");
    assert_eq!(letter.subject, "s");
    assert_eq!(letter.body, "hello\n\n- a list\n");
}

#[test]
fn crlf_line_ends_become_lf() {
    let letter = parse(b"Subject: s\r\n\r\nline one\r\nline two\r\n");
    assert_eq!(letter.body, "line one\nline two\n");
    assert_eq!(letter.kind, None);
    assert!(!letter.is_event());
}

#[test]
fn an_empty_event_header_is_not_an_event() {
    assert!(!parse(b"X-Vivi-Event:\nSubject: s\n\nbody").is_event());
}

#[test]
fn quoted_printable_decodes_with_soft_breaks() {
    let raw = b"Content-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: quoted-printable\n\ncaf=C3=A9 and a long=\n line =3D done\n";
    assert_eq!(parse(raw).body, "caf\u{e9} and a long line = done\n");
}

#[test]
fn base64_decodes() {
    // "h\u{e9}llo <b>" in UTF-8.
    let raw = b"Content-Type: text/plain\nContent-Transfer-Encoding: base64\n\naMOpbGxvIDxiPg==\n";
    assert_eq!(parse(raw).body, "h\u{e9}llo <b>");
}

#[test]
fn a_multipart_gives_its_plain_part_even_after_an_html_part() {
    let raw = b"Content-Type: multipart/alternative; boundary=\"XX\"\n\nignored preamble\n--XX\nContent-Type: text/html\n\n<p>html</p>\n--XX\nContent-Type: text/plain\n\nplain text\n--XX--\nepilogue\n";
    assert_eq!(parse(raw).body.trim_end(), "plain text");
}

#[test]
fn a_folded_header_is_unfolded() {
    let raw = b"Subject: s\nX-Vivi-Kind:\n task\n\nbody";
    assert_eq!(parse(raw).kind.as_deref(), Some("task"));
}

#[test]
fn a_letter_with_no_body_or_bad_bytes_still_reads() {
    assert_eq!(parse(b"").body, "");
    assert_eq!(parse(b"Subject: only headers").body, "");
    assert!(parse(b"Subject: s\n\nbad \xff byte").body.contains("bad"));
}

#[test]
fn a_non_text_part_has_no_text() {
    assert_eq!(parse(b"Content-Type: application/pdf\n\n%PDF").body, "");
}

#[test]
fn a_leading_front_matter_fence_is_dropped() {
    assert_eq!(strip_front("+++\na = 1\n+++\n\nbody\n"), "body\n");
    assert_eq!(strip_front("plain\n"), "plain\n");
    assert_eq!(strip_front("+++\nunclosed\n"), "+++\nunclosed\n");
    assert_eq!(strip_front("+++\n+++\n\nx"), "+++\n+++\n\nx");
}

#[test]
fn oldest_first_puts_undated_last_and_keeps_ties_in_order() {
    let mut items = [("c", None), ("b", Some(5)), ("a", Some(5)), ("z", Some(1))];
    oldest_first(&mut items, |item| item.1);
    let order: Vec<&str> = items.iter().map(|item| item.0).collect();
    assert_eq!(order, ["z", "b", "a", "c"]);
}

#[test]
fn a_letter_file_outside_the_mailspace_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["../x", "/etc/hosts", "a/../../x"] {
        assert!(read_blob(dir.path(), bad).is_err(), "{bad}");
        assert!(blob_path(dir.path(), bad).is_none(), "{bad}");
    }
    assert!(blob_path(dir.path(), "blobs/ab/cd.eml").is_some());
}

#[test]
fn a_big_letter_file_is_cut_at_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.eml"), vec![b'x'; 128 * 1024 + 10]).unwrap();
    std::fs::write(dir.path().join("fit.eml"), vec![b'x'; 128 * 1024]).unwrap();
    let big = read_blob(dir.path(), "big.eml").unwrap();
    assert_eq!((big.bytes.len() as u64, big.cut), (BLOB_MAX, true));
    let fit = read_blob(dir.path(), "fit.eml").unwrap();
    assert_eq!((fit.bytes.len() as u64, fit.cut), (BLOB_MAX, false));
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
