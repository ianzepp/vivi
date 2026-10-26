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
    assert_eq!(note.flow, None);
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
fn plain_prose_and_an_empty_front_are_not_notes() {
    assert_eq!(note_of("pump-attempt: t=1 hello"), None);
    assert_eq!(note_of("just words"), None);
    assert_eq!(note_of(""), None);
    let empty = crate::letter::render(&MailFront::default(), "words").unwrap();
    assert_eq!(note_of(&empty), None);
    assert_eq!(note_of(&task_body("a task")), None);
}

#[test]
fn a_run_start_note_carries_its_flow_in_front_matter() {
    let body = run_start_body(FlowRef::new("46e19522", "rec-91b6", 7), "run opened").unwrap();
    let note = note_of(&body).expect("a note");
    assert_eq!(note.pump.note, NoteWord::RunStart);
    assert_eq!(note.pump.reference, None);
    assert_eq!(note.flow, Some(FlowRef::new("46e19522", "rec-91b6", 7)));
    assert_eq!(note.prose.trim(), "run opened");
    assert!(body.contains("note = \"run-start\""), "{body}");
    assert!(body.contains("[flow]"), "{body}");
    assert!(!body.contains("X-Vivi"), "facts ride front matter: {body}");
    assert!(!body.contains("run-start:"), "no prose prefix: {body}");
}

#[test]
fn a_progress_note_round_trips_its_task() {
    let mut flow = FlowRef::new("46e19522", "rec-91b6", 7);
    flow.task = Some("91b691a3".to_string());
    let body = progress_body(flow.clone(), "slice merged").unwrap();
    let note = note_of(&body).expect("a note");
    assert_eq!(note.pump.note, NoteWord::Progress);
    assert_eq!(note.flow, Some(flow));
    assert_eq!(note.prose.trim(), "slice merged");
    assert!(body.contains("note = \"progress\""), "{body}");
    assert!(body.contains("task = \"91b691a3\""), "{body}");
}

#[test]
fn a_run_note_body_refuses_an_invalid_flow() {
    let mut flow = FlowRef::new("46e19522", "rec-91b6", 7);
    flow.run = 0;
    for body in [run_start_body(flow.clone(), "x"), progress_body(flow, "x")] {
        let err = body.err().map(|err| err.to_string()).unwrap_or_default();
        assert!(err.contains("flow.run"), "{err}");
    }
}

#[test]
fn a_run_note_without_a_flow_table_is_not_read_back() {
    let body = "+++\n[pump]\nnote = \"run-start\"\nt = 1\n+++\n\nno flow\n";
    assert_eq!(note_of(body), None);
    let body = "+++\n[pump]\nnote = \"progress\"\nt = 1\n+++\n\nno flow\n";
    assert_eq!(note_of(body), None);
}
