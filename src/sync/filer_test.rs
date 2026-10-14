use super::*;

#[test]
fn kinds_file_into_work_folders_and_unknown_kinds_degrade_to_inbox() {
    assert_eq!(role_for(Some("need")), "needs");
    assert_eq!(role_for(Some("TASK")), "tasks");
    assert_eq!(role_for(Some("want")), "wants");
    assert_eq!(role_for(None), "inbox");
    assert_eq!(role_for(Some("mail")), "inbox");
    assert_eq!(role_for(Some("proposal")), "inbox");
    assert_eq!(work_kind(Some("proposal")), None);
}

/// A record with an envelope `kind` and a letter that is (or is not) an
/// event.
fn event_record(kind: Option<&str>, event: bool) -> (Record, LetterHeaders) {
    (
        Record {
            sequence: 1,
            message_id: "msg_000000010000000000000000".into(),
            from: "mind@00112233.local".into(),
            to: Some("hand@00112233.local".into()),
            kind: kind.map(str::to_string),
            content_sha256: "ab".into(),
            from_prefix: None,
            appended_at: None,
            raw_eml: String::new(),
        },
        LetterHeaders {
            is_event: event,
            ..LetterHeaders::default()
        },
    )
}

#[test]
fn an_event_letter_sees_no_kind_whatever_the_envelope_carries() {
    let (record, headers) = event_record(Some("need"), true);
    assert_eq!(letter_kind(&record, &headers), None);
    assert_eq!(role_for(letter_kind(&record, &headers)), "inbox");
    assert_eq!(work_kind(letter_kind(&record, &headers)), None);

    let (work, headers) = event_record(Some("need"), false);
    assert_eq!(letter_kind(&work, &headers), Some("need"));
}
