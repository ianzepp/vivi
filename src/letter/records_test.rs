use super::*;
use crate::letter::{LetterError, parse, render};

fn round_trip<T>(front: &T) -> T
where
    T: Serialize + serde::de::DeserializeOwned + Validate,
{
    let text = render(front, "prose").expect("render");
    parse::<T>(&text).expect("parse").front
}

fn refused<T: serde::de::DeserializeOwned + Validate>(text: &str) -> LetterError {
    match parse::<T>(text) {
        Ok(_) => panic!("expected a refusal of: {text}"),
        Err(err) => err,
    }
}

fn invalid_naming<T: serde::de::DeserializeOwned + Validate>(text: &str, words: &[&str]) {
    match refused::<T>(text) {
        LetterError::Invalid(reason) => {
            for word in words {
                assert!(reason.contains(word), "{reason} lacks {word}");
            }
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
}

fn action(body: &str) -> String {
    format!("+++\nneed = \"abc\"\n{body}+++\n\nprose")
}

#[test]
fn record_round_trips_full_and_minimal() {
    let full = RecordFront {
        need: "n1".to_string(),
        kind: Some(RecordKind::Repair),
        target: Some("main".to_string()),
        tested: Some("deadbeef".to_string()),
    };
    assert_eq!(round_trip(&full), full);
    let minimal = RecordFront {
        need: "n1".to_string(),
        kind: None,
        target: None,
        tested: None,
    };
    assert_eq!(round_trip(&minimal), minimal);
    assert_eq!(
        render(&minimal, "x").expect("render"),
        "+++\nneed = \"n1\"\n+++\n\nx"
    );
}

#[test]
fn record_refusals() {
    assert!(matches!(
        refused::<RecordFront>("+++\nneed = \"n\"\nbogus = 1\n+++\n"),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<RecordFront>("+++\nkind = \"build\"\n+++\n"),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<RecordFront>("+++\nneed = \"n\"\nkind = \"deploy\"\n+++\n"),
        LetterError::Toml { .. }
    ));
    invalid_naming::<RecordFront>(
        "+++\nneed = \"n\"\nkind = \"build\"\ntested = \"abc\"\n+++\n",
        &["tested", "repair"],
    );
    invalid_naming::<RecordFront>("+++\nneed = \" \"\n+++\n", &["need"]);
    invalid_naming::<RecordFront>("+++\nneed = \"n\"\ntarget = \"\"\n+++\n", &["target"]);
}

#[test]
fn record_kind_maps_to_flow_kind() {
    assert_eq!(RecordKind::Build.flow_kind(), FlowKind::Build);
    assert_eq!(RecordKind::Research.flow_kind(), FlowKind::Research);
    assert_eq!(RecordKind::Release.flow_kind(), FlowKind::Release);
    assert_eq!(RecordKind::Repair.flow_kind(), FlowKind::Repair);
}

#[test]
fn action_round_trips_full_and_minimal() {
    let requeue = ActionFront {
        action: ActionWord::Requeue,
        need: "n".to_string(),
        unit: Some("u1".to_string()),
        attempts: Some(3),
        fresh: true,
        refund: true,
        force: false,
        step: None,
        word: None,
    };
    assert_eq!(round_trip(&requeue), requeue);
    let answer = ActionFront {
        action: ActionWord::Answer,
        need: "n".to_string(),
        unit: None,
        attempts: None,
        fresh: false,
        refund: false,
        force: false,
        step: Some("s".to_string()),
        word: Some("pass".to_string()),
    };
    assert_eq!(round_trip(&answer), answer);
    let replan = ActionFront {
        action: ActionWord::Replan,
        force: true,
        step: None,
        word: None,
        ..answer.clone()
    };
    assert_eq!(round_trip(&replan), replan);
    for word in [ActionWord::Withdraw, ActionWord::Unlock] {
        let bare = ActionFront {
            action: word,
            force: false,
            step: None,
            word: None,
            ..answer.clone()
        };
        assert_eq!(round_trip(&bare), bare);
    }
    let restart = ActionFront {
        action: ActionWord::Unlock,
        step: None,
        word: Some("restart".to_string()),
        ..answer
    };
    assert_eq!(round_trip(&restart), restart);
}

#[test]
fn action_structural_refusals() {
    assert!(matches!(
        refused::<ActionFront>(&action("action = \"recover\"\n")),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<ActionFront>("+++\naction = \"withdraw\"\n+++\n"),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<ActionFront>(&action("action = \"withdraw\"\nbogus = 1\n")),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<ActionFront>("+++\nneed = \"n\"\n+++\n"),
        LetterError::Toml { .. }
    ));
}

#[test]
fn action_conditionals_name_key_and_action() {
    invalid_naming::<ActionFront>(&action("action = \"requeue\"\n"), &["unit", "requeue"]);
    invalid_naming::<ActionFront>(
        &action("action = \"answer\"\nword = \"pass\"\n"),
        &["step", "answer"],
    );
    invalid_naming::<ActionFront>(
        &action("action = \"answer\"\nstep = \"s\"\n"),
        &["word", "answer"],
    );
    invalid_naming::<ActionFront>(
        &action("action = \"withdraw\"\nforce = true\n"),
        &["force", "withdraw"],
    );
    invalid_naming::<ActionFront>(
        &action("action = \"replan\"\nunit = \"u\"\n"),
        &["unit", "replan"],
    );
    invalid_naming::<ActionFront>(
        &action("action = \"unlock\"\nstep = \"s\"\n"),
        &["step", "unlock"],
    );
    invalid_naming::<ActionFront>(
        &action("action = \"answer\"\nstep = \"s\"\nword = \"w\"\nfresh = true\n"),
        &["fresh", "answer"],
    );
    invalid_naming::<ActionFront>(
        &action("action = \"withdraw\"\nattempts = 2\n"),
        &["attempts", "withdraw"],
    );
    invalid_naming::<ActionFront>(
        &action("action = \"withdraw\"\nrefund = true\n"),
        &["refund", "withdraw"],
    );
    invalid_naming::<ActionFront>(
        &action("action = \"requeue\"\nunit = \" \"\n"),
        &["unit", "blank"],
    );
}

#[test]
fn mail_round_trips_empty_and_noted() {
    let empty = MailFront::default();
    assert_eq!(round_trip(&empty), empty);
    assert_eq!(render(&empty, "hi").expect("render"), "+++\n+++\n\nhi");
    let noted = MailFront {
        pump: Some(PumpNote {
            note: NoteWord::RepairRung,
            t: 1_700_000_000_000_000,
            reference: Some("job-1 reason text".to_string()),
        }),
        job: None,
    };
    assert_eq!(round_trip(&noted), noted);
    let bare = MailFront {
        pump: Some(PumpNote {
            note: NoteWord::Result,
            t: 5,
            reference: None,
        }),
        job: None,
    };
    assert_eq!(round_trip(&bare), bare);
}

#[test]
fn every_note_word_round_trips() {
    for note in [
        NoteWord::Attempt,
        NoteWord::Brief,
        NoteWord::Deviations,
        NoteWord::FollowUp,
        NoteWord::SliceEnv,
        NoteWord::RepairAttempt,
        NoteWord::RepairRung,
        NoteWord::MainRed,
        NoteWord::Abandoned,
        NoteWord::SliceMerge,
        NoteWord::Result,
    ] {
        let front = MailFront {
            pump: Some(PumpNote {
                note,
                t: 1,
                reference: None,
            }),
            job: (note == NoteWord::Brief).then(|| JobFront {
                id: "j1".to_string(),
                kind: "build".to_string(),
                builtin_role: None,
            }),
        };
        assert_eq!(round_trip(&front), front);
    }
}

#[test]
fn mail_refusals() {
    assert!(matches!(
        refused::<MailFront>("+++\nbogus = 1\n+++\n"),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<MailFront>("+++\n[pump]\nnote = \"attempt\"\nt = 1\nbogus = 1\n+++\n"),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<MailFront>("+++\n[pump]\nnote = \"recover\"\nt = 1\n+++\n"),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<MailFront>("+++\n[pump]\nnote = \"attempt\"\n+++\n"),
        LetterError::Toml { .. }
    ));
    assert!(matches!(
        refused::<MailFront>("+++\n[pump]\nt = 1\n+++\n"),
        LetterError::Toml { .. }
    ));
    invalid_naming::<MailFront>("+++\n[pump]\nnote = \"attempt\"\nt = 0\n+++\n", &["pump.t"]);
    invalid_naming::<MailFront>(
        "+++\n[pump]\nnote = \"attempt\"\nt = 1\nref = \" \"\n+++\n",
        &["pump.ref"],
    );
}

#[test]
fn brief_note_carries_a_job() {
    let brief = MailFront {
        pump: Some(PumpNote {
            note: NoteWord::Brief,
            t: 9,
            reference: None,
        }),
        job: Some(JobFront {
            id: "j1".to_string(),
            kind: "build".to_string(),
            builtin_role: Some("hand".to_string()),
        }),
    };
    assert_eq!(round_trip(&brief), brief);
    invalid_naming::<MailFront>(
        "+++\n[pump]\nnote = \"attempt\"\nt = 1\n[job]\nid = \"j\"\nkind = \"k\"\n+++\n",
        &["job", "brief"],
    );
    invalid_naming::<MailFront>(
        "+++\n[job]\nid = \"j\"\nkind = \"k\"\n+++\n",
        &["job", "brief"],
    );
    invalid_naming::<MailFront>(
        "+++\n[pump]\nnote = \"brief\"\nt = 1\n+++\n",
        &["brief", "[job]"],
    );
    invalid_naming::<MailFront>(
        "+++\n[pump]\nnote = \"brief\"\nt = 1\n[job]\nid = \" \"\nkind = \"k\"\n+++\n",
        &["job.id"],
    );
}

#[test]
fn record_letter_lifts_the_prose_lines_into_the_front() {
    let (front, prose) = record_letter(
        "n1",
        "Fix it.\nkind: repair\ntarget: alpha\ntested: abc\nmore",
    )
    .unwrap();
    assert_eq!(
        front,
        RecordFront {
            need: "n1".to_string(),
            kind: Some(RecordKind::Repair),
            target: Some("alpha".to_string()),
            tested: Some("abc".to_string()),
        }
    );
    assert_eq!(prose, "Fix it.\nmore");
}

#[test]
fn record_letter_refuses_an_unknown_kind_and_tested_without_repair() {
    match record_letter("n1", "kind: frobnicate") {
        Err(LetterError::Invalid(reason)) => {
            assert!(
                reason.contains("build, research, release, repair"),
                "{reason}"
            );
            assert!(reason.contains("actual: frobnicate"), "{reason}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert!(matches!(
        record_letter("n1", "kind: build\ntested: abc"),
        Err(LetterError::Invalid(_))
    ));
    assert!(matches!(
        record_letter("n1", "tested: abc"),
        Err(LetterError::Invalid(_))
    ));
}

#[test]
fn record_letter_takes_a_front_matter_body_as_written() {
    let text = render(
        &RecordFront {
            need: "other".to_string(),
            kind: Some(RecordKind::Research),
            target: None,
            tested: None,
        },
        "prose",
    )
    .unwrap();
    let (front, prose) = record_letter("n1", &text).unwrap();
    assert_eq!(front.need, "other");
    assert_eq!(front.kind, Some(RecordKind::Research));
    assert_eq!(prose, "prose");
    assert!(record_letter("n1", "+++\nneed = \"x\"\nbogus = 1\n+++\n").is_err());
}

#[test]
fn record_letter_empty_lines_are_absent() {
    let (front, prose) = record_letter("n1", "kind:\ntarget:\nbody").unwrap();
    assert_eq!(front.kind, None);
    assert_eq!(front.target, None);
    assert_eq!(prose, "body");
}
