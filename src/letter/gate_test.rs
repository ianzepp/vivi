//! Unit tests for the storage-free letter gate.

use super::gate;
use crate::letter::fixture::{need_body, task_body_with};
use crate::letter::records::{MailFront, RecordFront};
use crate::letter::work::{NeedFront, TaskFront};
use crate::letter::{Front, LetterKind, render};

const HAND_ENVELOPE: &str =
    "+++\nanswer = \"built\"\nchange = []\ndeviations = []\nremaining = []\n+++\n\nall done\n";

#[test]
fn a_need_that_opens_without_a_fence_is_refused_with_todays_message() {
    let err = gate("need", "done_when: x\n").unwrap_err().to_string();
    assert_eq!(
        err,
        "need has no done_when: pass --done-when or add done_when = [...] to the front matter"
    );
}

#[test]
fn a_need_without_done_when_in_its_front_matter_is_refused() {
    let body = render(&NeedFront::default(), "prose").unwrap();
    let err = gate("need", &body).unwrap_err().to_string();
    assert!(err.contains("need has no done_when"), "{err}");
}

#[test]
fn a_want_that_opens_without_a_fence_is_refused_with_todays_message() {
    let err = gate("want", "what if\n").unwrap_err().to_string();
    assert_eq!(
        err,
        "want has no question: pass --question or add question = \"...\" to the front matter"
    );
}

#[test]
fn a_task_that_opens_without_a_fence_is_refused_with_todays_message() {
    let err = gate("task", "do it\n").unwrap_err().to_string();
    assert_eq!(
        err,
        "task letter has no front matter: open the body with +++ ... +++ (an empty front matter is fine)"
    );
}

#[test]
fn a_front_matter_need_is_formatted_and_parses() {
    let gated = gate("need", &need_body(&["done"], "prose")).unwrap();
    assert_eq!(gated.kind, "need");
    assert!(gated.formatted);
    assert!(gated.depends_on.is_empty());
    let letter = gated.letter.expect("need parses");
    assert_eq!(letter.kind, LetterKind::Need);
}

#[test]
fn a_task_body_extracts_its_dependencies() {
    let body = task_body_with(
        &TaskFront {
            depends_on: vec!["abc".into(), "def".into()],
            ..TaskFront::default()
        },
        "prose",
    );
    let gated = gate("task", &body).unwrap();
    assert!(gated.formatted);
    assert_eq!(gated.depends_on, vec!["abc".to_string(), "def".to_string()]);
}

#[test]
fn a_prose_mail_passes_as_written_and_formatted() {
    let gated = gate("mail", "hello there").unwrap();
    assert_eq!(gated.kind, "mail");
    assert!(gated.formatted);
    assert!(gated.depends_on.is_empty());
    let letter = gated.letter.expect("prose mail parses");
    assert_eq!(letter.kind, LetterKind::Mail);
    assert_eq!(letter.front, Front::Mail(MailFront::default()));
    assert_eq!(letter.prose, "hello there");
}

#[test]
fn a_prose_chat_passes_as_written_and_formatted() {
    let gated = gate("chat", "hello there").unwrap();
    assert!(gated.formatted);
    let letter = gated.letter.expect("prose chat parses");
    assert_eq!(letter.front, Front::Chat);
    assert_eq!(letter.prose, "hello there");
}

#[test]
fn a_fenced_mail_body_with_an_unknown_front_key_is_refused() {
    let err = gate("mail", "+++\ncolour = \"red\"\n+++\n\nhi\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("colour"), "{err}");
    assert!(err.contains("not a role reply envelope"), "{err}");
}

#[test]
fn an_action_front_passes_as_formatted_mail() {
    let gated = gate(
        "mail",
        "+++\naction = \"withdraw\"\nneed = \"abc\"\n+++\n\nstop\n",
    )
    .unwrap();
    assert!(gated.formatted);
    let letter = gated.letter.expect("action parses");
    assert_eq!(letter.kind, LetterKind::Action);
}

#[test]
fn a_role_reply_envelope_passes_as_formatted_mail() {
    let gated = gate("mail", HAND_ENVELOPE).unwrap();
    assert!(gated.formatted);
    let letter = gated.letter.expect("envelope parses");
    assert!(matches!(letter.kind, LetterKind::Reply(_)));
}

#[test]
fn a_pump_record_body_parses_formatted() {
    let body = render(
        &RecordFront {
            need: "abc".into(),
            kind: None,
            target: None,
            tested: None,
        },
        "run it",
    )
    .unwrap();
    let gated = gate("pump", &body).unwrap();
    assert_eq!(gated.kind, "pump");
    assert!(gated.formatted);
    assert!(gated.depends_on.is_empty());
    let letter = gated.letter.expect("record parses");
    assert_eq!(letter.kind, LetterKind::Record);
    assert!(matches!(letter.front, Front::Record(_)));
}

#[test]
fn a_pump_record_with_a_prose_body_is_refused() {
    let err = gate("pump", "Dispatch of a need.").unwrap_err().to_string();
    assert!(err.contains("front matter fence"), "{err}");
}

#[test]
fn an_unknown_kind_passes_unchanged_and_unformatted() {
    let gated = gate("memo", "plain note").unwrap();
    assert_eq!(gated.kind, "memo");
    assert!(!gated.formatted);
    assert!(gated.depends_on.is_empty());
    assert!(gated.letter.is_none());
}
