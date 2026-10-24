use super::*;
use crate::letter::Front;
use crate::letter::plain::{self, CloseFront, EmptyFront, MemoFront, RuleFront, RuleScope};
use crate::message::{ComposeDraft, build_compose_draft};
use std::collections::BTreeMap;

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

#[test]
fn a_blank_format_header_gets_its_own_message() {
    let err = of_message(
        &stored("X-Vivi-Format:\r\nX-Vivi-Kind: need\r\n", NEED),
        "h1",
    )
    .unwrap_err();
    assert_eq!(err, LetterError::UnknownFormat(String::new()));
    assert!(err.to_string().contains("blank `X-Vivi-Format`"), "{err}");
}

#[test]
fn a_memo_letter_round_trips_its_tags_through_the_wire() {
    let body = plain::render(
        &MemoFront {
            tags: vec!["ops".to_string(), "wave3".to_string()],
        },
        "the note",
    )
    .unwrap();
    let raw = stored("X-Vivi-Format: 1\r\nX-Vivi-Kind: memo\r\n", &body);
    let letter = of_message(&raw, "h1").expect("letter");
    assert_eq!(letter.kind, LetterKind::Memo);
    assert!(matches!(&letter.front, Front::Memo(front) if front.tags == ["ops", "wave3"]));
    assert_eq!(letter.prose, "the note");
}

#[test]
fn a_rule_letter_round_trips_scope_and_supersedes_through_the_wire() {
    let body = plain::render(
        &RuleFront {
            scope: Some(RuleScope::Project),
            supersedes: Some("abc123".to_string()),
        },
        "the rule",
    )
    .unwrap();
    let raw = stored("X-Vivi-Format: 1\r\nX-Vivi-Kind: rule\r\n", &body);
    let letter = of_message(&raw, "h1").expect("letter");
    assert_eq!(letter.kind, LetterKind::Rule);
    assert!(matches!(&letter.front, Front::Rule(front)
            if front.scope == Some(RuleScope::Project)
                && front.supersedes.as_deref() == Some("abc123")));
    assert_eq!(letter.prose, "the rule");
}

#[test]
fn a_done_event_letter_round_trips_its_close_front_through_the_wire() {
    let mut repos = BTreeMap::new();
    repos.insert("vivi".to_string(), "abc1234".to_string());
    let front = CloseFront {
        verdict: Some("clean".to_string()),
        branch: Some("w3/x".to_string()),
        commit: Some("0123abc".to_string()),
        repos,
    };
    let body = plain::render(&front, "closed it").unwrap();
    let headers = "X-Vivi-Format: 1\r\nX-Vivi-Kind: mail\r\nX-Vivi-Event: done\r\n";
    let letter = of_message(&stored(headers, &body), "h1").expect("letter");
    assert_eq!(letter.kind, LetterKind::Event("done".to_string()));
    let Front::Event(close) = &letter.front else {
        panic!("expected an event front");
    };
    assert_eq!(close.verdict.as_deref(), Some("clean"));
    assert_eq!(close.branch.as_deref(), Some("w3/x"));
    assert_eq!(close.commit.as_deref(), Some("0123abc"));
    assert_eq!(close.repos.get("vivi").map(String::as_str), Some("abc1234"));
    assert_eq!(letter.prose, "closed it");
}

#[test]
fn letters_with_an_empty_front_carry_no_fence_through_the_wire() {
    let bodies = [
        plain::render(&MemoFront::default(), "plain note").unwrap(),
        plain::render(&RuleFront::default(), "plain note").unwrap(),
        plain::render(&CloseFront::default(), "plain note").unwrap(),
        plain::render(&EmptyFront {}, "plain note").unwrap(),
    ];
    for body in &bodies {
        assert_eq!(body, "plain note");
        assert!(!body.contains("+++"));
    }
    let memo = of_message(
        &stored("X-Vivi-Format: 1\r\nX-Vivi-Kind: memo\r\n", &bodies[0]),
        "h",
    )
    .unwrap();
    assert_eq!(memo.kind, LetterKind::Memo);
    assert_eq!(memo.front, Front::Memo(MemoFront::default()));
    assert_eq!(memo.prose, "plain note");
    let rule = of_message(
        &stored("X-Vivi-Format: 1\r\nX-Vivi-Kind: rule\r\n", &bodies[1]),
        "h",
    )
    .unwrap();
    assert_eq!(rule.kind, LetterKind::Rule);
    assert_eq!(rule.front, Front::Rule(RuleFront::default()));
    assert_eq!(rule.prose, "plain note");
    let done = of_message(
        &stored(
            "X-Vivi-Format: 1\r\nX-Vivi-Kind: mail\r\nX-Vivi-Event: done\r\n",
            &bodies[2],
        ),
        "h",
    )
    .unwrap();
    assert_eq!(done.kind, LetterKind::Event("done".to_string()));
    assert_eq!(done.front, Front::Event(CloseFront::default()));
    assert_eq!(done.prose, "plain note");
    let marked = of_message(
        &stored(
            "X-Vivi-Format: 1\r\nX-Vivi-Kind: mail\r\nX-Vivi-Event: marked\r\n",
            &bodies[3],
        ),
        "h",
    )
    .unwrap();
    assert_eq!(marked.kind, LetterKind::Event("marked".to_string()));
    assert_eq!(marked.front, Front::Event(CloseFront::default()));
    assert_eq!(marked.prose, "plain note");
}
