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
fn parse_headers_flags_verdict_headers() {
    let verdict = letter("X-Vivi-Verdict: fixed\r\n");
    let repo = letter("X-Vivi-Repo: vivi=sha9\r\n");
    let plain = letter("");
    assert!(parse_headers(&verdict).has_verdict);
    assert!(parse_headers(&repo).has_verdict);
    assert!(!parse_headers(&plain).has_verdict);
}
