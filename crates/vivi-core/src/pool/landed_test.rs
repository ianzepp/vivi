use super::*;
use crate::mailspace::SendRequest;

fn space_with_task() -> (tempfile::TempDir, Mailspace, String) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let body = vivi_mail::letter::fixture::task_body_with(
        &vivi_mail::letter::work::TaskFront {
            from: Some("main".into()),
            branch: Some("factory/x".into()),
            ..Default::default()
        },
        "do it",
    );
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Fix the edge".into(),
            body,
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    (tmp, space, handle)
}

fn record(job: &str, landing: Landing) -> LandedRecord {
    LandedRecord {
        job: job.into(),
        landing,
        base: None,
        tip: None,
        secs: None,
        command: None,
        tail: None,
        files: Vec::new(),
    }
}

#[test]
fn the_note_has_exactly_the_documented_fields_and_omits_absent_ones() {
    let bare = record("j1", Landing::Landed);
    let note: Value = serde_json::from_str(&landed_note(&bare)).unwrap();
    assert_eq!(note, serde_json::json!({"job": "j1", "landing": "landed"}));

    let full = LandedRecord {
        base: Some("aaa".into()),
        tip: Some("bbb".into()),
        secs: Some(42),
        command: Some("cargo test".into()),
        tail: Some("boom".into()),
        files: vec!["a.rs".into(), "b.rs".into()],
        ..record("j1", Landing::Red)
    };
    let note: Value = serde_json::from_str(&landed_note(&full)).unwrap();
    assert_eq!(
        note,
        serde_json::json!({
            "job": "j1", "landing": "red", "base": "aaa", "tip": "bbb", "secs": 42,
            "command": "cargo test", "tail": "boom", "files": ["a.rs", "b.rs"],
        })
    );
}

#[test]
fn the_tail_keeps_the_end_on_a_character_boundary() {
    let short = "x".repeat(TAIL_CAP);
    assert_eq!(cap_tail(&short), short);
    let long = format!("HEAD{}END", "é".repeat(TAIL_CAP));
    let kept = cap_tail(&long);
    assert!(kept.len() <= TAIL_CAP);
    assert!(kept.len() > TAIL_CAP - 2);
    assert!(kept.ends_with("END"));
    assert!(!kept.contains("HEAD"));

    let red = LandedRecord {
        tail: Some(long),
        ..record("j1", Landing::Red)
    };
    let note: Value = serde_json::from_str(&landed_note(&red)).unwrap();
    assert!(note["tail"].as_str().unwrap().len() <= TAIL_CAP);
}

#[test]
fn only_the_four_words_parse() {
    for word in ["landed", "red", "conflict", "raced"] {
        assert_eq!(Landing::parse(word).unwrap().word(), word);
    }
    assert!(Landing::parse("merged").is_err());
    assert!(Landing::parse("").is_err());
}

#[test]
fn the_text_names_the_job_outcome_and_reason() {
    let record = LandedRecord {
        base: Some("a".repeat(40)),
        secs: Some(7),
        command: Some("cargo test".into()),
        tail: Some("boom\n".into()),
        ..record("j1", Landing::Red)
    };
    let text = landed_text(&record);
    assert!(text.starts_with("pool landing j1: red"), "{text}");
    assert!(text.contains(&format!("base {}", "a".repeat(12))), "{text}");
    assert!(text.contains("7s"), "{text}");
    assert!(text.contains("failed: cargo test"), "{text}");
    assert!(text.ends_with("--- tail\nboom"), "{text}");
}

#[test]
fn a_landing_records_an_event_on_the_task_for_a_dispatched_job() {
    let (_tmp, space, handle) = space_with_task();
    space
        .record_pool_note(&handle, "pool_dispatched", r#"{"job":"j1"}"#, "go")
        .unwrap();

    record_landed(&space, &handle, &record("j1", Landing::Landed)).unwrap();

    let events = space
        .storage()
        .unwrap()
        .list_mailspace_events_of_types(&[LANDED_EVENT])
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].note.as_deref(),
        Some(r#"{"job":"j1","landing":"landed"}"#)
    );
}

#[test]
fn a_job_the_task_never_dispatched_is_refused() {
    let (_tmp, space, handle) = space_with_task();
    let err = record_landed(&space, &handle, &record("j1", Landing::Landed)).unwrap_err();
    assert!(err.to_string().contains("no dispatch of job j1"), "{err}");

    space
        .record_pool_note(&handle, "pool_dispatched", r#"{"job":"j1"}"#, "go")
        .unwrap();
    let err = record_landed(&space, &handle, &record("j2", Landing::Landed)).unwrap_err();
    assert!(err.to_string().contains("no dispatch of job j2"), "{err}");
}

#[test]
fn a_landing_is_not_a_pool_event_for_the_open_dispatch_fold() {
    let (_tmp, space, handle) = space_with_task();
    space
        .record_pool_note(&handle, "pool_dispatched", r#"{"job":"j1"}"#, "go")
        .unwrap();
    record_landed(&space, &handle, &record("j1", Landing::Landed)).unwrap();
    let events = space.pool_events().unwrap();
    assert_eq!(events.len(), 1, "pool_events lists no landing");
    assert!(
        !crate::mailspace::POOL_EVENT_TYPES.contains(&LANDED_EVENT),
        "a landing must not join the events the open-dispatch fold reads"
    );
}
