use super::*;

#[test]
fn an_unaddressed_raw_letter_does_not_match_delivery_watchers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    let raw = format!(
        "From: mind@{}\r\nSubject: only\r\nMessage-ID: <only@x.test>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n",
        mailspace.address_domain()
    );
    let result = mailspace.deliver_raw(raw.as_bytes(), "inbox").unwrap();
    let storage = mailspace.storage().unwrap();
    let id = storage
        .resolve_message_token(result.sent.as_deref().unwrap())
        .unwrap();
    let events = storage.list_mailspace_events(&id).unwrap();
    for identity in ["mind", "hand"] {
        let filters = WatchFilters {
            identity: [identity.to_string()].into_iter().collect(),
            kinds: ["mail".to_string()].into_iter().collect(),
            events: ["delivered".to_string()].into_iter().collect(),
            statuses: None,
            match_from: None,
            subject_prefix: None,
            content_id: None,
        };
        assert!(
            !events
                .iter()
                .any(|event| matches_event(&storage, event, &filters).unwrap()),
            "recipient-delivery watcher for {identity} matched {events:?}"
        );
    }
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "sent_copy_created")
    );
}

#[test]
fn parses_watch_durations() {
    assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
    assert_eq!(parse_duration("2s").unwrap(), Duration::from_secs(2));
    assert!(parse_duration("soon").is_err());
}

#[test]
fn rejects_unknown_filter_values() {
    assert!(parse_filter_set("task,wat", "kind", &["mail", "task"]).is_err());
}

fn event(command: &str, event_type: &str, to_role: &str) -> MailspaceEvent {
    MailspaceEvent {
        event_id: 1,
        occurred_at: "2026-10-11T00:00:00Z".into(),
        command: command.into(),
        event_type: event_type.into(),
        actor_identity: None,
        account: "hand".into(),
        message_id: "msg".into(),
        content_id: "content".into(),
        from_role: None,
        to_role: Some(to_role.into()),
        from_identity: None,
        to_identity: Some("hand".into()),
        subject: "s".into(),
        note: None,
    }
}

#[test]
fn local_events_take_their_kind_from_the_command() {
    assert_eq!(
        event_kind(&event("need send", "delivered", "needs")),
        "need"
    );
    assert_eq!(event_kind(&event("task done", "moved", "done")), "task");
    assert_eq!(
        event_kind(&event("mail send", "delivered", "inbox")),
        "mail"
    );
}

#[test]
fn a_pulled_letter_takes_its_kind_from_the_role_it_was_delivered_to() {
    for (role, kind) in [
        ("needs", "need"),
        ("tasks", "task"),
        ("wants", "want"),
        ("inbox", "mail"),
        ("sent", "mail"),
    ] {
        assert_eq!(
            event_kind(&event("sync pull", "delivered", role)),
            kind,
            "{role}"
        );
    }
}
