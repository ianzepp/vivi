use super::{rule_headers, state_name, successor_note};
use crate::storage::MailspaceEvent;

fn event(event_type: &str, note: Option<&str>) -> MailspaceEvent {
    MailspaceEvent {
        event_id: 0,
        occurred_at: String::new(),
        command: String::new(),
        event_type: event_type.to_string(),
        actor_identity: None,
        account: String::new(),
        message_id: String::new(),
        content_id: String::new(),
        from_role: None,
        to_role: None,
        from_identity: None,
        to_identity: None,
        subject: String::new(),
        note: note.map(str::to_string),
    }
}

#[test]
fn state_prefers_superseded_then_retired_then_absorbed() {
    let superseded = vec![event("superseded", Some("msg_abc"))];
    assert_eq!(state_name(&superseded, true, false), "superseded");
    assert_eq!(state_name(&[], false, true), "superseded");
    assert_eq!(
        state_name(&[event("retired", None)], true, false),
        "retired"
    );
    assert_eq!(state_name(&[], true, false), "absorbed");
    assert_eq!(state_name(&[], false, false), "active");
    assert_eq!(successor_note(&superseded), Some("msg_abc"));
}

#[test]
fn headers_read_local_scope_and_the_supersedes_id() {
    let raw = "\
Subject: one\r\n\
X-Vivi-Kind: rule\r\n\
X-Vivi-Rule-Scope: local\r\n\
X-Vivi-Supersedes: msg_old\r\n\
\r\n\
body\r\n";
    let headers = rule_headers(raw.as_bytes());
    assert_eq!(headers.scope, "local");
    assert_eq!(headers.supersedes.as_deref(), Some("msg_old"));

    let project = rule_headers(b"Subject: two\r\nX-Vivi-Rule-Scope: project\r\n\r\nbody\r\n");
    assert_eq!(project.scope, "project");
    assert!(project.supersedes.is_none());
}
