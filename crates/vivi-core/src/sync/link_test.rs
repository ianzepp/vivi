//! Header-reading companions for [`crate::sync::link`].

use crate::sync::link::parse_headers;

fn letter(extra: &str) -> Vec<u8> {
    format!(
        "Message-ID: <child@example.test>\r\n\
         From: hand@example.test\r\n\
         To: operator@example.test\r\n\
         In-Reply-To: <parent@example.test>\r\n\
         Subject: Re: a report\r\n\
         {extra}\r\n\
         a body\r\n"
    )
    .into_bytes()
}

#[test]
fn parse_headers_flags_close_facts_in_front_matter() {
    let closed = letter(
        "X-Vivi-Format: 1\r\nX-Vivi-Event: done\r\n\r\n\
         +++\r\nverdict = \"fixed\"\r\n+++\r\n\r\ndone\r\n",
    );
    let located = letter(
        "X-Vivi-Format: 1\r\nX-Vivi-Event: done\r\n\r\n\
         +++\r\nbranch = \"mind/review\"\r\ncommit = \"deadbeef\"\r\n+++\r\n\r\ndone\r\n",
    );
    let plain = letter("");
    assert!(parse_headers(&closed).has_verdict);
    assert!(parse_headers(&located).has_verdict);
    assert!(!parse_headers(&plain).has_verdict);
}

#[test]
fn parse_headers_flags_a_report_notes_close_front() {
    let reported = letter(
        "X-Vivi-Format: 1\r\nX-Vivi-Kind: mail\r\n\r\n\
         +++\r\n[close]\r\nverdict = \"fixed\"\r\n[close.repos]\r\nvivi = \"sha9\"\r\n+++\r\n\r\ndone\r\n",
    );
    let plain_note = letter("X-Vivi-Format: 1\r\nX-Vivi-Kind: mail\r\n\r\ndone\r\n");
    let empty_close = letter(
        "X-Vivi-Format: 1\r\nX-Vivi-Kind: mail\r\n\r\n+++\r\n[close]\r\n+++\r\n\r\ndone\r\n",
    );
    assert!(parse_headers(&reported).has_verdict);
    assert!(!parse_headers(&plain_note).has_verdict);
    assert!(!parse_headers(&empty_close).has_verdict);
}

#[test]
fn parse_headers_ignores_legacy_close_headers() {
    let legacy = letter("X-Vivi-Verdict: fixed\r\n");
    let legacy_repo = letter("X-Vivi-Repo: vivi=sha9\r\n");
    // A close with no facts carries none, format 1 or not.
    let unlocated = letter("X-Vivi-Format: 1\r\nX-Vivi-Event: done\r\n\r\ndone\r\n");
    assert!(!parse_headers(&legacy).has_verdict);
    assert!(!parse_headers(&legacy_repo).has_verdict);
    assert!(!parse_headers(&unlocated).has_verdict);
}
