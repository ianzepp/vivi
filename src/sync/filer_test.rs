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

#[test]
fn a_verdict_letter_is_marked_touched_for_the_settle() {
    let c = crate::sync::fake_server::checkout("alice", &["mind"]);
    let filer = Filer::new(&c.mailspace, &c.settings);
    let storage = c.mailspace.storage().unwrap();
    let to = c.address("mind");
    let letter = format!(
        "Message-ID: <r1@example.com>\r\nFrom: hand@example.com\r\nTo: {to}\r\n\
         In-Reply-To: <task@example.com>\r\nX-Vivi-Format: 1\r\nX-Vivi-Event: done\r\n\r\n\
         +++\r\nverdict = \"fixed\"\r\n+++\r\n\r\ndone: the unit landed\r\n"
    );
    let bytes = letter.into_bytes();
    let record = Record {
        sequence: 1,
        message_id: "msg_000000010000000000000000".into(),
        from: "hand@example.com".into(),
        to: Some(to.clone()),
        kind: None,
        content_sha256: sha256_hex(&bytes),
        from_prefix: None,
        appended_at: None,
        raw_eml: b64::encode(&bytes),
    };
    let mut report = PullReport::default();
    filer
        .file_record(&storage, &record, &mut report, &mut Vec::new())
        .unwrap();
    assert_eq!(report.touched, vec![sha256_hex(&bytes)]);

    // A legacy verdict letter (headers, no format) is history: not touched.
    let legacy = format!(
        "Message-ID: <r2@example.com>\r\nFrom: hand@example.com\r\nTo: {to}\r\n\
         In-Reply-To: <task@example.com>\r\nX-Vivi-Verdict: fixed\r\n\r\n\
         done: the unit landed\r\n"
    );
    let bytes = legacy.into_bytes();
    let record = Record {
        sequence: 2,
        message_id: "msg_000000020000000000000000".into(),
        from: "hand@example.com".into(),
        to: Some(to),
        kind: None,
        content_sha256: sha256_hex(&bytes),
        from_prefix: None,
        appended_at: None,
        raw_eml: b64::encode(&bytes),
    };
    let mut report = PullReport::default();
    filer
        .file_record(&storage, &record, &mut report, &mut Vec::new())
        .unwrap();
    assert!(report.touched.is_empty(), "{:?}", report.touched);
}
