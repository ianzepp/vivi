use super::*;

#[test]
fn a_plain_letter_gives_its_body_kind_and_event() {
    let raw =
        b"From: a@x\nX-Vivi-Kind: Need\nX-Vivi-Event: Done\nSubject: s\n\nhello\n\n- a list\n";
    let letter = read_letter(raw);
    assert_eq!(letter.kind.as_deref(), Some("need"));
    assert_eq!(letter.event.as_deref(), Some("done"));
    assert_eq!(letter.body, "hello\n\n- a list\n");
}

#[test]
fn crlf_line_ends_become_lf() {
    let letter = read_letter(b"Subject: s\r\n\r\nline one\r\nline two\r\n");
    assert_eq!(letter.body, "line one\nline two\n");
    assert_eq!(letter.kind, None);
}

#[test]
fn quoted_printable_decodes_with_soft_breaks() {
    let raw = b"Content-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: quoted-printable\n\ncaf=C3=A9 and a long=\n line =3D done\n";
    assert_eq!(read_letter(raw).body, "caf\u{e9} and a long line = done\n");
}

#[test]
fn base64_decodes() {
    // "h\u{e9}llo <b>" in UTF-8.
    let raw = b"Content-Type: text/plain\nContent-Transfer-Encoding: base64\n\naMOpbGxvIDxiPg==\n";
    assert_eq!(read_letter(raw).body, "h\u{e9}llo <b>");
}

#[test]
fn a_multipart_gives_its_plain_part_even_after_an_html_part() {
    let raw = b"Content-Type: multipart/alternative; boundary=\"XX\"\n\nignored preamble\n--XX\nContent-Type: text/html\n\n<p>html</p>\n--XX\nContent-Type: text/plain\n\nplain text\n--XX--\nepilogue\n";
    assert_eq!(read_letter(raw).body, "plain text\n");
}

#[test]
fn a_folded_header_is_unfolded() {
    let raw = b"Subject: s\nX-Vivi-Kind:\n task\n\nbody";
    assert_eq!(read_letter(raw).kind.as_deref(), Some("task"));
}

#[test]
fn a_letter_with_no_body_or_bad_bytes_still_reads() {
    assert_eq!(read_letter(b"").body, "");
    assert_eq!(read_letter(b"Subject: only headers").body, "");
    let letter = read_letter(b"Subject: s\n\nbad \xff byte");
    assert!(letter.body.contains("bad"));
}

#[test]
fn a_non_text_part_has_no_text() {
    let raw = b"Content-Type: application/pdf\n\n%PDF";
    assert_eq!(read_letter(raw).body, "");
}
