use super::{folded_marks, validate_mark};
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
fn fold_keeps_the_latest_op_per_slug_and_sorts() {
    let events = vec![
        event("marked", Some("regression")),
        event("marked", Some("bug")),
        event("delivered", Some("bug")),
        event("unmarked", Some("regression")),
        event("marked", Some("Bug")),
        event("marked", Some("bug")),
    ];
    assert_eq!(folded_marks(&events), vec!["Bug".to_string(), "bug".into()]);
}

#[test]
fn fold_unmark_removes_only_that_slug() {
    let events = vec![
        event("marked", Some("bug")),
        event("marked", Some("regression")),
        event("unmarked", Some("bug")),
    ];
    assert_eq!(folded_marks(&events), vec!["regression".to_string()]);
}

#[test]
fn fold_ignores_an_unmark_of_an_absent_slug() {
    assert!(folded_marks(&[event("unmarked", Some("bug"))]).is_empty());
}

#[test]
fn mark_slugs_use_the_role_label_character_set() {
    assert!(validate_mark("bug").is_ok());
    assert!(validate_mark("Bug").is_ok());
    assert!(validate_mark("bug.fix-1_a").is_ok());
    let spaced = validate_mark("bug fix").unwrap_err().to_string();
    assert!(
        spaced.contains("invalid mark 'bug fix'; use letters, numbers, dot, dash, or underscore"),
        "{spaced}"
    );
    assert!(validate_mark("").is_err());
}
