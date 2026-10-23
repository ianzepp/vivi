use super::*;
use crate::mailspace::{Mailspace, SendRequest};

fn event(
    id: i64,
    at: &str,
    event_type: &str,
    to_role: Option<&str>,
    note: Option<&str>,
) -> MailspaceEvent {
    MailspaceEvent {
        event_id: id,
        occurred_at: at.to_string(),
        command: String::new(),
        event_type: event_type.to_string(),
        actor_identity: None,
        account: String::new(),
        message_id: String::new(),
        content_id: String::new(),
        from_role: None,
        to_role: to_role.map(str::to_string),
        from_identity: None,
        to_identity: None,
        subject: String::new(),
        note: note.map(str::to_string),
    }
}

fn item(events: Vec<MailspaceEvent>) -> HeldItem {
    HeldItem {
        rows: Vec::new(),
        events,
    }
}

#[test]
fn a_closed_item_is_done() {
    let closed = item(vec![event(
        1,
        "2026-01-01T00:00:01Z",
        "moved",
        Some("done"),
        None,
    )]);
    assert!(is_done(&closed));
}

#[test]
fn a_closed_then_reopened_item_is_open() {
    let reopened = item(vec![
        event(2, "2026-01-01T00:00:02Z", "moved", Some("needs"), None),
        event(1, "2026-01-01T00:00:01Z", "moved", Some("done"), None),
    ]);
    assert!(!is_done(&reopened));
    assert_eq!(
        newest_move(&reopened.events).map(|event| event.event_id),
        Some(2)
    );
}

#[test]
fn an_item_with_no_move_and_no_rows_is_open() {
    assert!(!is_done(&item(Vec::new())));
}

#[test]
fn marks_fold_in_event_order_not_list_order() {
    let marked_last = item(vec![
        event(2, "2026-01-01T00:00:02Z", "marked", None, Some("bug")),
        event(1, "2026-01-01T00:00:01Z", "unmarked", None, Some("bug")),
    ]);
    assert_eq!(sorted_marks(&marked_last), vec!["bug".to_string()]);
    let unmarked_last = item(vec![
        event(1, "2026-01-01T00:00:01Z", "marked", None, Some("bug")),
        event(2, "2026-01-01T00:00:02Z", "unmarked", None, Some("bug")),
    ]);
    assert!(sorted_marks(&unmarked_last).is_empty());
}

#[test]
fn a_held_item_reads_rows_and_marks_from_a_real_mailspace() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    let need = mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec!["mind".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "fix auth".into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    mailspace
        .set_marks("mind", &need, "need", true, &["bug".to_string()])
        .unwrap();
    let storage = mailspace.storage().unwrap();
    let held = held_item(&storage, &need).unwrap();
    assert!(!held.rows.is_empty());
    assert!(!is_done(&held));
    assert_eq!(sorted_marks(&held), vec!["bug".to_string()]);
    assert!(held_item(&storage, "no-such-handle").is_err());
}
