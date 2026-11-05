use super::*;

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

use crate::mailspace::{Mailspace, MailspaceWatchRequest, SendRequest, run_watch};

fn author_space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

/// A watch that matches only when a scanned event passes every filter.
fn watch(for_identity: &str, events: &str) -> MailspaceWatchRequest {
    MailspaceWatchRequest {
        for_identity: for_identity.into(),
        kinds: "mail,task,need,want".into(),
        events: events.into(),
        statuses: None,
        match_from: None,
        match_subject_prefix: None,
        handle: None,
        until_count: 1,
        // A zero timeout makes one scan and then fails: Ok means a match.
        timeout: Some("0s".into()),
        once: false,
        since: None,
        cursor_file: None,
        write_cursor: false,
        poll_interval: "10ms".into(),
        json: false,
    }
}

#[test]
fn a_recipientless_send_wakes_no_recipient_watcher() {
    let (_tmp, mailspace) = author_space();
    mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "note to self".into(),
            body: "kept for me".into(),
            role: "inbox".into(),
            kind: Some("mail".into()),
            reply_to: None,
        })
        .unwrap();

    for role in ["hand", "mind"] {
        assert!(
            run_watch(&mailspace, watch(role, "delivered")).is_err(),
            "a delivery watch misses this send for {role}"
        );
    }
    assert!(
        run_watch(&mailspace, watch("mind", "sent_copy_created")).is_ok(),
        "the author's own sent event still wakes"
    );
}
