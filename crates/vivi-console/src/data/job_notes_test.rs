use super::*;

fn event(kind: &str, note: &str) -> TaskEvent {
    TaskEvent {
        at: 0,
        event_type: kind.into(),
        command: String::new(),
        actor: None,
        note: Some(note.into()),
    }
}

fn letter(text: &str) -> Letter {
    Letter {
        body: text.into(),
        ..Letter::default()
    }
}

#[test]
fn records_are_the_newest_pool_notes_that_name_the_job() {
    let events = [
        event("pool_dispatched", r#"{"job":"j1","n":1}"#),
        event("pool_dispatched", r#"{"job":"j2","n":2}"#),
        event("pool_collected", r#"{"job":"j1","outcome":"finished"}"#),
        event("pool_landed", r#"{"job":"j1","landing":"red"}"#),
        event("pool_landed", r#"{"job":"j1","landing":"landed"}"#),
        event("done", r#"{"job":"j1"}"#),
    ];
    let records = records_of(&events, "j1");
    assert_eq!(records.dispatched.as_deref(), Some(r#"{"job":"j1","n":1}"#));
    assert!(records.collected.unwrap().contains("finished"));
    assert!(records.landed.unwrap().contains("\"landed\""));
    assert_eq!(records_of(&events, "j3"), JobRecords::default());
}

#[test]
fn a_note_that_is_not_json_names_no_job_and_prints_as_it_is() {
    let events = [event("pool_collected", "not json")];
    assert_eq!(records_of(&events, "j1"), JobRecords::default());
    assert_eq!(pretty("not json"), "not json");
    assert!(pretty(r#"{"a":1}"#).contains("\n  \"a\": 1"));
}

#[test]
fn text_fields_read_strings_and_numbers_but_not_empties() {
    let note = r#"{"a":"x","n":7,"e":"","o":{}}"#;
    assert_eq!(text_field(note, "a").as_deref(), Some("x"));
    assert_eq!(text_field(note, "n").as_deref(), Some("7"));
    assert_eq!(text_field(note, "e"), None);
    assert_eq!(text_field(note, "o"), None);
    assert_eq!(text_field(note, "missing"), None);
}

#[test]
fn a_landing_note_reads_every_field_of_the_contract() {
    let note = r#"{"job":"j","landing":"red","base":"aaa","tip":"bbb","secs":"12",
        "command":"cargo test","tail":"boom","files":["a.rs","b.rs"]}"#;
    let landing = landing_of(note).unwrap();
    assert_eq!(landing.word, "red");
    assert_eq!(landing.base.as_deref(), Some("aaa"));
    assert_eq!(landing.tip.as_deref(), Some("bbb"));
    assert_eq!(landing.secs, Some(12));
    assert_eq!(landing.command.as_deref(), Some("cargo test"));
    assert_eq!(landing.tail.as_deref(), Some("boom"));
    assert_eq!(landing.files, ["a.rs", "b.rs"]);
    let bare = landing_of(r#"{"job":"j","landing":"landed"}"#).unwrap();
    assert!(bare.files.is_empty() && bare.command.is_none() && bare.secs.is_none());
    assert_eq!(landing_of(r#"{"job":"j"}"#), None);
    assert_eq!(landing_of("nope"), None);
}

const RECEIPT: &str = "pool result j1: finished\npool main  slot slot-1\nagent exit 0\n\n\
    --- facts\nrustc 1.80\n\n--- receipt\nAll done.\n\nTwo paragraphs.\n\n\
    --- stderr tail\nwarning: x\n\n--- runner log tail\nlog 1\nlog 2\n";

#[test]
fn a_receipt_splits_into_its_sections() {
    let receipt = receipt_of(&[letter(RECEIPT)], "j1").unwrap();
    assert!(receipt.head.starts_with("pool result j1: finished"));
    assert!(receipt.head.ends_with("agent exit 0"));
    assert_eq!(receipt.facts.as_deref(), Some("rustc 1.80"));
    assert_eq!(
        receipt.agent.as_deref(),
        Some("All done.\n\nTwo paragraphs.")
    );
    assert_eq!(receipt.stderr.as_deref(), Some("warning: x"));
    assert_eq!(receipt.runner_log.as_deref(), Some("log 1\nlog 2"));
}

#[test]
fn the_receipt_is_the_one_for_this_job() {
    let other = letter("pool result j2: timeout\nno result");
    let dispatch = letter("pool dispatch j1\npool main");
    let replies = [dispatch, other, letter(RECEIPT)];
    assert!(receipt_of(&replies, "j1").unwrap().agent.is_some());
    assert!(receipt_of(&replies, "j2").unwrap().agent.is_none());
    assert_eq!(receipt_of(&replies, "j3"), None);
}

#[test]
fn a_dashed_line_that_is_not_a_known_section_stays_in_the_text() {
    let text = "pool result j1: finished\n\n--- receipt\nfirst\n\n--- not a section\nsecond\n";
    let receipt = receipt_of(&[letter(text)], "j1").unwrap();
    assert_eq!(
        receipt.agent.as_deref(),
        Some("first\n\n--- not a section\nsecond")
    );
}
