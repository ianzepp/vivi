use super::*;
use crate::letter::records::{ActionWord, MailFront};

fn letter(front: &str, prose: &str) -> String {
    format!("+++\n{front}+++\n\n{prose}\n")
}

fn kind_of(kind: &str) -> Result<LetterKind, LetterError> {
    LetterKind::from_headers(Some(kind), None)
}

#[test]
fn from_headers_reads_every_kind_word() {
    for (word, expected) in [
        ("need", LetterKind::Need),
        ("want", LetterKind::Want),
        ("task", LetterKind::Task),
        ("memo", LetterKind::Memo),
        ("rule", LetterKind::Rule),
        ("pump", LetterKind::Record),
        ("mail", LetterKind::Mail),
        ("chat", LetterKind::Chat),
    ] {
        assert_eq!(kind_of(word), Ok(expected.clone()));
        assert_eq!(
            kind_of(&format!("  {}  ", word.to_uppercase())),
            Ok(expected)
        );
    }
}

#[test]
fn a_missing_or_blank_kind_header_is_unknown_with_an_empty_word() {
    for kind in [None, Some(""), Some("  ")] {
        let err = LetterKind::from_headers(kind, None).unwrap_err();
        assert_eq!(err, LetterError::UnknownKind(String::new()));
        assert!(err.to_string().contains("X-Vivi-Kind"));
    }
    assert_eq!(
        kind_of("zap"),
        Err(LetterError::UnknownKind("zap".to_string()))
    );
    assert_eq!(
        LetterKind::from_headers(Some("mail"), Some("  ")),
        Ok(LetterKind::Mail)
    );
}

#[test]
fn an_event_verb_wins_over_the_kind_header() {
    let expected = Ok(LetterKind::Event("done".to_string()));
    assert_eq!(
        LetterKind::from_headers(Some("mail"), Some(" Done ")),
        expected
    );
    assert_eq!(
        LetterKind::from_headers(Some("zap"), Some("done")),
        expected
    );
    assert_eq!(LetterKind::from_headers(None, Some("done")), expected);
}

#[test]
fn header_names_the_kind_a_writer_puts_on_the_letter() {
    for (kind, header) in [
        (LetterKind::Need, Some("need")),
        (LetterKind::Want, Some("want")),
        (LetterKind::Task, Some("task")),
        (LetterKind::Memo, Some("memo")),
        (LetterKind::Rule, Some("rule")),
        (LetterKind::Record, Some("pump")),
        (LetterKind::Action, Some("mail")),
        (LetterKind::Event("marked".to_string()), Some("mail")),
        (LetterKind::Mail, Some("mail")),
        (LetterKind::Chat, Some("chat")),
        (LetterKind::Reply(RoleKind::Hand), None),
    ] {
        assert_eq!(kind.header(), header);
    }
}

#[test]
fn parse_kind_gives_the_matching_front_per_kind() {
    let cases = [
        (LetterKind::Need, "done_when = [\"ships\"]\n"),
        (LetterKind::Want, "question = \"why\"\n"),
        (LetterKind::Task, ""),
        (LetterKind::Memo, "tags = [\"a\"]\n"),
        (LetterKind::Rule, "scope = \"local\"\n"),
        (LetterKind::Record, "need = \"abc\"\n"),
        (
            LetterKind::Action,
            "action = \"withdraw\"\nneed = \"abc\"\n",
        ),
        (LetterKind::Event("done".to_string()), "verdict = \"ok\"\n"),
        (LetterKind::Mail, ""),
        (LetterKind::Chat, ""),
    ];
    for (kind, front) in cases {
        let parsed = parse_kind(&kind, &letter(front, "the prose")).unwrap();
        assert_eq!(parsed.kind, kind);
        assert_eq!(parsed.prose, "the prose");
        let matches = match (&kind, &parsed.front) {
            (LetterKind::Need, Front::Need(_))
            | (LetterKind::Want, Front::Want(_))
            | (LetterKind::Task, Front::Task(_))
            | (LetterKind::Memo, Front::Memo(_))
            | (LetterKind::Rule, Front::Rule(_))
            | (LetterKind::Record, Front::Record(_))
            | (LetterKind::Action, Front::Action(_))
            | (LetterKind::Event(_), Front::Event(_))
            | (LetterKind::Mail, Front::Mail(_))
            | (LetterKind::Chat, Front::Chat) => true,
            _ => false,
        };
        assert!(matches, "{kind:?} gave {:?}", parsed.front);
    }
}

#[test]
fn a_need_keeps_table_lines_in_its_prose() {
    let prose = "Reply like:\n[[check]]\ncmd = \"x\"\n[[follow_up]]\nneed = 'y'";
    let parsed = parse_kind(
        &LetterKind::Need,
        &letter("done_when = [\"ships\"]\n", prose),
    )
    .unwrap();
    assert_eq!(parsed.prose, prose);
}

#[test]
fn a_hand_reply_still_refuses_a_table_below_the_fence() {
    let text = letter("answer = \"built\"\n", "[[check]]\ncmd = \"x\"");
    assert!(matches!(
        parse_kind(&LetterKind::Reply(RoleKind::Hand), &text),
        Err(LetterError::TableBelowFence { .. })
    ));
}

#[test]
fn a_hand_reply_parses_as_a_reply() {
    let text = letter(
        "answer = \"built\"\nchange = [\"src/a.rs\"]\n\n[[check]]\ncmd = \"cargo test\"\nresult = \"pass\"\nseconds = 94\n",
        "report",
    );
    let parsed = parse_kind(&LetterKind::Reply(RoleKind::Hand), &text).unwrap();
    assert_eq!(parsed.kind, LetterKind::Reply(RoleKind::Hand));
    assert_eq!(parsed.prose, "report");
    let Front::Reply(reply) = parsed.front else {
        panic!("expected a reply front");
    };
    assert_eq!(reply.answer, "built");
}

#[test]
fn mail_with_an_action_key_becomes_an_action() {
    let text = letter("action = \"withdraw\"\nneed = \"abc\"\n", "stop");
    let parsed = parse_kind(&LetterKind::Mail, &text).unwrap();
    assert_eq!(parsed.kind, LetterKind::Action);
    let Front::Action(action) = parsed.front else {
        panic!("expected an action front");
    };
    assert_eq!(action.action, ActionWord::Withdraw);
}

#[test]
fn mail_with_only_a_pump_note_stays_mail() {
    let text = letter("[pump]\nnote = \"attempt\"\nt = 5\n", "note");
    let parsed = parse_kind(&LetterKind::Mail, &text).unwrap();
    assert_eq!(parsed.kind, LetterKind::Mail);
    assert!(matches!(parsed.front, Front::Mail(_)));
}

#[test]
fn a_broken_mail_front_reports_the_mail_error() {
    assert_eq!(
        parse_kind(&LetterKind::Mail, "+++\nunclosed"),
        Err(LetterError::Unclosed)
    );
    assert!(matches!(
        parse_kind(&LetterKind::Mail, &letter("action = = 1\n", "x")),
        Err(LetterError::Toml { .. })
    ));
}

#[test]
fn close_events_take_a_verdict_and_other_events_take_none() {
    let done = LetterKind::Event("done".to_string());
    let parsed = parse_kind(&done, &letter("verdict = \"ok\"\n", "x")).unwrap();
    let Front::Event(close) = parsed.front else {
        panic!("expected an event front");
    };
    assert_eq!(close.verdict.as_deref(), Some("ok"));

    let marked = LetterKind::Event("marked".to_string());
    assert!(matches!(
        parse_kind(&marked, &letter("verdict = \"ok\"\n", "x")),
        Err(LetterError::Toml { .. })
    ));
    let parsed = parse_kind(&marked, &letter("", "x")).unwrap();
    assert_eq!(parsed.front, Front::Event(CloseFront::default()));
}

#[test]
fn mail_with_a_role_envelope_is_a_reply() {
    let text = letter(
        "answer = \"built\"\nchange = []\ndeviations = []\nremaining = []\n",
        "done",
    );
    let parsed = parse_kind(&LetterKind::Mail, &text).unwrap();
    assert_eq!(parsed.kind, LetterKind::Reply(RoleKind::Hand));
    assert_eq!(parsed.prose, "done");
    assert!(matches!(parsed.front, Front::Reply(ref reply) if reply.answer == "built"));
}

#[test]
fn mail_with_keys_that_fit_no_front_is_refused_naming_both() {
    let err = parse_kind(&LetterKind::Mail, &letter("colour = \"red\"\n", "x"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("colour"), "{err}");
    assert!(
        err.contains("(and the front matter is not a role reply envelope)"),
        "{err}"
    );
}

#[test]
fn a_role_envelope_that_fails_its_own_rules_is_refused() {
    let err = parse_kind(
        &LetterKind::Mail,
        &letter("answer = \"blocked-scope\"\n", "x"),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("(and the front matter is not a role reply envelope)"),
        "{err}"
    );
}

#[test]
fn every_role_is_listed_once() {
    let all = RoleKind::ALL;
    for (index, role) in all.iter().enumerate() {
        assert!(!all[index + 1..].contains(role), "{role:?} listed twice");
    }
}

#[test]
fn a_mail_or_chat_with_no_front_matter_is_prose_with_an_empty_front() {
    let mail = parse_kind(&LetterKind::Mail, "\nhello there\n").unwrap();
    assert_eq!(mail.kind, LetterKind::Mail);
    assert_eq!(mail.front, Front::Mail(MailFront::default()));
    assert_eq!(mail.prose, "hello there");
    let chat = parse_kind(&LetterKind::Chat, "hi").unwrap();
    assert_eq!(chat.front, Front::Chat);
    assert_eq!(chat.prose, "hi");
}

#[test]
fn a_wrapped_mail_and_chat_body_still_parses() {
    let mail = parse_kind(&LetterKind::Mail, "+++\n+++\n\nold").unwrap();
    assert_eq!(mail.front, Front::Mail(MailFront::default()));
    assert_eq!(mail.prose, "old");
    let chat = parse_kind(&LetterKind::Chat, "+++\n+++\n\nold").unwrap();
    assert_eq!(chat.front, Front::Chat);
    assert_eq!(chat.prose, "old");
}

#[test]
fn an_invalid_chat_front_is_still_refused() {
    assert!(parse_kind(&LetterKind::Chat, "+++\ncolour = \"red\"\n+++\n\nhi").is_err());
}
