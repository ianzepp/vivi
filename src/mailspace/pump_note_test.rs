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
    let a = note_of(&note_body(NoteWord::Result, None, "").unwrap()).unwrap();
    let b = note_of(&note_body(NoteWord::Result, None, "").unwrap()).unwrap();
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
fn plain_prose_and_an_empty_front_are_not_notes() {
    assert_eq!(note_of("pump-attempt: t=1 hello"), None);
    assert_eq!(note_of("just words"), None);
    assert_eq!(note_of(""), None);
    let empty = crate::letter::render(&MailFront::default(), "words").unwrap();
    assert_eq!(note_of(&empty), None);
    assert_eq!(note_of(&task_body("a task")), None);
}
