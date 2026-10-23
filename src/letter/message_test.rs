use super::*;
use crate::letter::Front;
use crate::message::{ComposeDraft, build_compose_draft};

fn compose(body: &str) -> String {
    build_compose_draft(&ComposeDraft {
        from: "a@example.com".to_string(),
        to: vec!["b@example.com".to_string()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "subject".to_string(),
        body: body.to_string(),
        html_body: None,
    })
    .expect("compose")
}

fn stored(headers: &str, body: &str) -> Vec<u8> {
    format!("{headers}{}", compose(body)).into_bytes()
}

const FORMAT_ONE: &str = "X-Vivi-Format: 1\r\nX-Vivi-Kind: need\r\n";
const NEED: &str = "+++\ndone_when = [\"it works\"]\n+++\n\nBuild the thing.\n";

#[test]
fn a_format_one_need_reads_its_front_and_prose() {
    let letter = of_message(&stored(FORMAT_ONE, NEED), "h1").expect("letter");
    assert_eq!(letter.kind, LetterKind::Need);
    assert!(matches!(&letter.front, Front::Need(front) if front.done_when == ["it works"]));
    assert_eq!(letter.prose, "Build the thing.");
}

#[test]
fn a_letter_without_a_format_header_is_legacy() {
    let err = of_message(&stored("X-Vivi-Kind: Need\r\n", NEED), "h1").unwrap_err();
    assert_eq!(
        err,
        LetterError::Legacy {
            handle: "h1".to_string(),
            kind: "need".to_string()
        }
    );
    assert!(err.to_string().contains("h1"));
    let plain = of_message(&stored("", "hello"), "h2").unwrap_err();
    assert!(matches!(plain, LetterError::Legacy { kind, .. } if kind == "mail"));
}

#[test]
fn a_newer_format_is_unknown_format_not_legacy() {
    let raw = stored("X-Vivi-Format: 2\r\nX-Vivi-Kind: need\r\n", NEED);
    let err = of_message(&raw, "h1").unwrap_err();
    assert_eq!(err, LetterError::UnknownFormat("2".to_string()));
    assert!(err.to_string().contains("reads format 1"));
    assert!(!matches!(err, LetterError::Legacy { .. }));
}

#[test]
fn an_unknown_or_missing_kind_is_unknown_kind() {
    let zap = stored("X-Vivi-Format: 1\r\nX-Vivi-Kind: zap\r\n", NEED);
    assert_eq!(
        of_message(&zap, "h1"),
        Err(LetterError::UnknownKind("zap".to_string()))
    );
    let none = stored("X-Vivi-Format: 1\r\n", NEED);
    assert_eq!(
        of_message(&none, "h1"),
        Err(LetterError::UnknownKind(String::new()))
    );
}

#[test]
fn a_check_table_in_the_prose_is_not_refused() {
    let body = format!("{NEED}\n[[check]]\ncmd = \"cargo test\"\n");
    assert!(of_message(&stored(FORMAT_ONE, &body), "h1").is_ok());
}

#[test]
fn front_matter_errors_surface() {
    let unknown = "+++\ndone_when = [\"x\"]\nbogus = 1\n+++\n\nbody\n";
    assert!(matches!(
        of_message(&stored(FORMAT_ONE, unknown), "h1"),
        Err(LetterError::Toml { .. })
    ));
    assert_eq!(
        of_message(&stored(FORMAT_ONE, "just prose"), "h1"),
        Err(LetterError::NoOpening)
    );
}

#[test]
fn an_event_letter_reads_as_an_event() {
    let headers = "X-Vivi-Format: 1\r\nX-Vivi-Kind: mail\r\nX-Vivi-Event: done\r\n";
    let body = "+++\nverdict = \"pass\"\n+++\n\nall green\n";
    let letter = of_message(&stored(headers, body), "h1").expect("letter");
    assert_eq!(letter.kind, LetterKind::Event("done".to_string()));
    assert!(
        matches!(&letter.front, Front::Event(front) if front.verdict.as_deref() == Some("pass"))
    );
}

#[test]
fn an_encoded_body_is_decoded_before_parsing() {
    let prose = "caf\u{e9} \u{2014} ".to_string()
        + &"long line of prose ".repeat(20).trim_end().to_string();
    let body = format!("+++\ndone_when = [\"ok\"]\n+++\n\n{prose}\n");
    let letter = of_message(&stored(FORMAT_ONE, &body), "h1").expect("letter");
    assert_eq!(letter.prose, prose);
}

#[test]
fn garbage_bytes_never_parse_as_a_letter() {
    // mail_parser accepts any bytes as a message, so `Invalid` is only the
    // defensive branch; garbage ends at the header check or the front matter.
    let err = of_message(&[0xff, 0xfe, 0x00, 0x01], "h1").unwrap_err();
    assert!(matches!(err, LetterError::Legacy { .. }), "{err:?}");
    let mut raw = FORMAT_ONE.as_bytes().to_vec();
    raw.extend_from_slice(b"\r\n\xff\xfe\x00\x01");
    assert_eq!(of_message(&raw, "h1"), Err(LetterError::NoOpening));
}
