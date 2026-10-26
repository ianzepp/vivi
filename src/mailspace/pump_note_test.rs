use super::*;
use crate::letter::fixture::task_body;

#[test]
fn a_note_body_round_trips_through_note_of() {
    let body = note_body(NoteWord::Attempt, Some("job-1"), "first line\nsecond line").unwrap();
    let note = note_of(&body).expect("a note");
    assert_eq!(note.pump.note, NoteWord::Attempt);
    assert_eq!(note.pump.reference.as_deref(), Some("job-1"));
    assert!(note.pump.t > 0);
    assert_eq!(note.job, None);
    assert_eq!(note.prose.trim(), "first line\nsecond line");
}

#[test]
fn a_note_without_a_reference_has_none() {
    let body = note_body(NoteWord::Deviations, None, "x").unwrap();
    assert_eq!(note_of(&body).unwrap().pump.reference, None);
}

#[test]
fn stamps_strictly_increase() {
    let mut last = next_stamp();
    for _ in 0..200 {
        let next = next_stamp();
        assert!(next > last, "{next} after {last}");
        last = next;
    }
    let a = note_of(&note_body(NoteWord::Deviations, None, "").unwrap()).unwrap();
    let b = note_of(&note_body(NoteWord::Deviations, None, "").unwrap()).unwrap();
    assert!(b.pump.t > a.pump.t);
}

#[test]
fn a_brief_note_is_refused() {
    let err = note_body(NoteWord::Brief, None, "x")
        .unwrap_err()
        .to_string();
    assert!(err.contains("brief_body"), "{err}");
}

#[test]
fn a_brief_body_round_trips_with_its_job() {
    let job = JobFront {
        id: "h1".into(),
        kind: "merge".into(),
        builtin_role: Some("hand".into()),
    };
    let note = note_of(&brief_body(job.clone(), "WRITE SCOPE: a.rs").unwrap()).expect("a note");
    assert_eq!(note.pump.note, NoteWord::Brief);
    assert_eq!(note.pump.reference, None);
    assert_eq!(note.job, Some(job));
    assert_eq!(note.prose.trim(), "WRITE SCOPE: a.rs");
}

#[test]
fn a_flow_note_round_trips_through_note_of() {
    let flow = FlowRef {
        need: "n1".into(),
        record: "<record@x>".into(),
        run: 2,
        task: Some("t1".into()),
    };
    let body = flow_note_body(NoteWord::RunStart, flow.clone(), "run opened").unwrap();
    let note = note_of(&body).expect("a note");
    assert_eq!(note.pump.note, NoteWord::RunStart);
    assert_eq!(note.flow, Some(flow));
    assert_eq!(note.job, None);
    assert_eq!(note.pump.reference, None);
    assert_eq!(note.prose.trim(), "run opened");
}

#[test]
fn a_progress_note_names_its_flow() {
    let flow = FlowRef {
        need: "n1".into(),
        record: "<record@x>".into(),
        run: 3,
        task: None,
    };
    let body = flow_note_body(NoteWord::Progress, flow.clone(), "half done").unwrap();
    let note = note_of(&body).expect("a note");
    assert_eq!(note.pump.note, NoteWord::Progress);
    assert_eq!(note.flow, Some(flow));
}

#[test]
fn only_run_notes_carry_a_flow_and_run_notes_demand_one() {
    let flow = FlowRef {
        need: "n1".into(),
        record: "r".into(),
        run: 1,
        task: None,
    };
    let err = flow_note_body(NoteWord::Attempt, flow, "x")
        .unwrap_err()
        .to_string();
    assert!(err.contains("note_body"), "{err}");
    for word in [NoteWord::RunStart, NoteWord::Progress] {
        let err = note_body(word, None, "x").unwrap_err().to_string();
        assert!(err.contains("[flow]"), "{err}");
    }
}

#[test]
fn plain_prose_and_an_empty_front_are_not_notes() {
    assert_eq!(note_of("pump-attempt: t=1 hello"), None);
    assert_eq!(note_of("just words"), None);
    assert_eq!(note_of(""), None);
    let empty = crate::letter::render(&MailFront::default(), "words").unwrap();
    assert_eq!(note_of(&empty), None);
    assert_eq!(note_of(&task_body("a task")), None);
}
