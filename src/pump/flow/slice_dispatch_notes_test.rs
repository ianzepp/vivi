//! Tests for `SliceMail::notes` and `notes_from` on the real port and the fake.

use super::SliceMail;
use crate::letter::fixture::{step_body, task_body_with};
use crate::letter::records::NoteWord;
use crate::letter::work::TaskFront;
use crate::mailspace::pump_note::note_body;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::mail::{MailWriter, MailspacePort};

fn prose_of(notes: &[(String, super::Note)]) -> Vec<String> {
    notes
        .iter()
        .map(|(_, note)| note.prose.trim().to_owned())
        .collect()
}

#[test]
fn the_real_port_reads_notes_of_one_word_oldest_first() {
    use crate::pump::fixture::project;

    let p = project();
    let port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: "mind".into(),
    };
    let task = port
        .send_step(
            "mind",
            "operator",
            "slice",
            &task_body_with(
                &TaskFront {
                    target: Some("vivi".into()),
                    ..TaskFront::step("n1", 1, "slice:a")
                },
                "",
            ),
            &[],
        )
        .unwrap();
    let note = |word, prose: &str| note_body(word, None, prose).unwrap();
    port.reply(&task, &note(NoteWord::Deviations, "first"))
        .unwrap();
    port.reply(&task, &note(NoteWord::Attempt, "other word"))
        .unwrap();
    port.reply(&task, "plain prose reply").unwrap();
    port.reply(&task, "pump-deviations: old form").unwrap();
    port.reply(&task, &note(NoteWord::Deviations, "second"))
        .unwrap();
    port.reply(&task, &note(NoteWord::Deviations, "third"))
        .unwrap();

    let found = port.notes(&task, NoteWord::Deviations).unwrap();
    assert_eq!(prose_of(&found), ["first", "second", "third"]);
    assert!(
        found
            .windows(2)
            .all(|pair| pair[0].1.pump.t < pair[1].1.pump.t)
    );
    let attempts = port.notes(&task, NoteWord::Attempt).unwrap();
    assert_eq!(prose_of(&attempts), ["other word"]);
    assert!(port.notes(&task, NoteWord::SliceEnv).unwrap().is_empty());
}

#[test]
fn the_real_port_notes_from_keeps_only_the_senders() {
    use crate::pump::fixture::project;

    let p = project();
    let port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: "mind".into(),
    };
    let task = port
        .send_step("mind", "mind", "t", &step_body("n1", 1, "slice:a", ""), &[])
        .unwrap();
    port.reply(
        &task,
        &note_body(NoteWord::Deviations, None, "from mind").unwrap(),
    )
    .unwrap();
    p.mailspace
        .reply(
            &task,
            "operator",
            vec!["mind".into()],
            Vec::new(),
            None,
            note_body(NoteWord::Deviations, None, "from operator").unwrap(),
        )
        .unwrap();

    assert_eq!(
        prose_of(
            &port
                .notes_from(&task, NoteWord::Deviations, "mind")
                .unwrap()
        ),
        ["from mind"]
    );
    assert_eq!(
        prose_of(
            &port
                .notes_from(&task, NoteWord::Deviations, "operator")
                .unwrap()
        ),
        ["from operator"]
    );
    assert_eq!(port.notes(&task, NoteWord::Deviations).unwrap().len(), 2);
}

#[test]
fn the_fake_reads_notes_oldest_first_and_by_sender() {
    let mut mail = FakeMail::new("mind");
    let task = mail
        .send_step("mind", "mind", "t", &step_body("n1", 1, "slice:a", ""), &[])
        .unwrap();
    let note = |word, prose: &str| note_body(word, None, prose).unwrap();
    mail.reply(&task, &note(NoteWord::Deviations, "first"))
        .unwrap();
    mail.reply(&task, &note(NoteWord::Attempt, "other word"))
        .unwrap();
    mail.reply(&task, "plain prose reply").unwrap();
    mail.reply(&task, &note(NoteWord::Deviations, "second"))
        .unwrap();
    mail.foreign_reply(&task, "operator", &note(NoteWord::Deviations, "theirs"));
    mail.foreign_reply(&task, "operator", &note(NoteWord::Attempt, "theirs, other"));

    assert_eq!(
        prose_of(&mail.notes(&task, NoteWord::Deviations).unwrap()),
        ["first", "second"]
    );
    assert_eq!(
        prose_of(
            &mail
                .notes_from(&task, NoteWord::Deviations, "mind")
                .unwrap()
        ),
        ["first", "second"]
    );
    assert_eq!(
        prose_of(
            &mail
                .notes_from(&task, NoteWord::Deviations, "operator")
                .unwrap()
        ),
        ["theirs"]
    );
    assert!(
        mail.notes_from(&task, NoteWord::Deviations, "nobody")
            .unwrap()
            .is_empty()
    );
}
