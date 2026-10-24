use super::{rule_front, rule_scope, state_name, successor_note};
use crate::letter::plain::{RuleFront, RuleScope};
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

/// A stored format-1 rule letter around the prose a [`RuleFront`] renders to.
fn rule_letter(front: &RuleFront) -> Vec<u8> {
    let prose = crate::letter::plain::render(front, "body").unwrap();
    format!(
        "Subject: one\r\nX-Vivi-Kind: rule\r\nX-Vivi-Format: 1\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\n{prose}"
    )
    .into_bytes()
}

#[test]
fn front_matter_carries_the_scope_and_the_supersedes_id() {
    let local = rule_letter(&RuleFront {
        scope: Some(RuleScope::Local),
        supersedes: Some("msg_old".into()),
    });
    let front = rule_front(&local);
    assert_eq!(front.scope, Some(RuleScope::Local));
    assert_eq!(front.supersedes.as_deref(), Some("msg_old"));
    assert_eq!(rule_scope(&local), "local");

    let project = rule_letter(&RuleFront {
        scope: Some(RuleScope::Project),
        supersedes: None,
    });
    assert_eq!(rule_scope(&project), "project");
    assert!(rule_front(&project).supersedes.is_none());
}

#[test]
fn a_rule_letter_with_an_empty_front_reads_as_project_scope() {
    let bare = rule_letter(&RuleFront::default());
    let text = String::from_utf8_lossy(&bare);
    assert!(!text.contains("+++"), "{text}");
    assert_eq!(rule_front(&bare), RuleFront::default());
    assert_eq!(rule_scope(&bare), "project");
}

#[test]
fn a_legacy_rule_letters_headers_are_not_read_on_fresh_parse() {
    let legacy = b"Subject: one\r\n\
X-Vivi-Kind: rule\r\n\
X-Vivi-Rule-Scope: local\r\n\
X-Vivi-Supersedes: msg_old\r\n\
\r\n\
body\r\n";
    assert_eq!(rule_front(legacy), RuleFront::default());
    assert_eq!(rule_scope(legacy), "project");
}
