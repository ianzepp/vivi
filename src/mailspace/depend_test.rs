use super::*;
use crate::mailspace::{GraphNodeView, RoleUpdate, SendRequest};

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    mailspace.add_identity("boss").unwrap();
    mailspace
        .set_role(
            "boss",
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    (mailspace, tmp)
}

/// Send an item from `ceo` to `cto`; returns the sender's handle (the copy
/// `ceo` owns) and the recipient's.
fn send_item(
    mailspace: &Mailspace,
    kind: &str,
    subject: &str,
    depends_on: &[&String],
) -> (String, String) {
    let result = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "body".into(),
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: depends_on.iter().map(|h| (*h).clone()).collect(),
        })
        .unwrap();
    (result.sent, result.delivered[0].handle.clone())
}

fn node(mailspace: &Mailspace, handle: &str) -> GraphNodeView {
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(handle).unwrap();
    let shown = storage.display_handle(&id).unwrap();
    drop(storage);
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| shown.starts_with(&n.source_id) || n.source_id.starts_with(&shown))
        .unwrap_or_else(|| panic!("no node for {handle}"))
}

fn letters(mailspace: &Mailspace) -> usize {
    mailspace.storage().unwrap().list_messages().unwrap().len()
}

fn strings(values: &[&String]) -> Vec<String> {
    values.iter().map(|v| (*v).clone()).collect()
}

#[test]
fn author_adds_a_dependency_after_the_send() {
    let (mailspace, _tmp) = roster();
    let (dep, dep_held) = send_item(&mailspace, "need", "work", &[]);
    let (release, held) = send_item(&mailspace, "need", "release", &[]);
    assert_eq!(node(&mailspace, &held).readiness, "ready");

    let writes = mailspace
        .set_dependencies("ceo", &release, "need", &strings(&[&dep]), &[])
        .unwrap();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].added && writes[0].changed);
    assert_eq!(node(&mailspace, &held).readiness, "blocked");
    let listed = mailspace.backlog_dependencies(&release).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, "open");

    mailspace.backlog_complete_item(&dep_held).unwrap();
    assert_eq!(node(&mailspace, &held).readiness, "ready");
    assert_eq!(
        mailspace.backlog_dependencies(&release).unwrap()[0].state,
        "done"
    );
}

#[test]
fn author_removes_a_dependency_named_at_send() {
    let (mailspace, _tmp) = roster();
    let (dep, _) = send_item(&mailspace, "need", "work", &[]);
    let (release, held) = send_item(&mailspace, "need", "release", &[&dep]);
    assert_eq!(node(&mailspace, &held).readiness, "blocked");

    let writes = mailspace
        .set_dependencies("ceo", &release, "need", &[], &strings(&[&dep]))
        .unwrap();
    assert!(!writes[0].added && writes[0].changed);
    assert_eq!(node(&mailspace, &held).readiness, "ready");
    assert!(mailspace.backlog_dependencies(&release).unwrap().is_empty());

    // The header still names it; a later attach or repair must not put
    // the edge back.
    let storage = mailspace.storage().unwrap();
    let id = storage.resolve_message_token(&release).unwrap();
    let bytes = storage.read_message(&id).unwrap();
    drop(storage);
    mailspace
        .backlog_attach_letter("need", &id, "release", &bytes)
        .unwrap();
    assert!(mailspace.backlog_dependencies(&release).unwrap().is_empty());
    mailspace.backlog_audit(true).unwrap();
    assert!(mailspace.backlog_dependencies(&release).unwrap().is_empty());

    // Adding it back is the newest word again.
    mailspace
        .set_dependencies("ceo", &release, "need", &strings(&[&dep]), &[])
        .unwrap();
    assert_eq!(node(&mailspace, &held).readiness, "blocked");
}

#[test]
fn task_dependencies_follow_the_letters() {
    let (mailspace, _tmp) = roster();
    let (dep, _) = send_item(&mailspace, "task", "first", &[]);
    let (task, delivered) = send_item(&mailspace, "task", "second", &[&dep]);
    let storage = mailspace.storage().unwrap();
    let delivered_id = storage.resolve_message_token(&delivered).unwrap();
    drop(storage);
    assert_eq!(mailspace.task_depends_on(&delivered_id).unwrap().len(), 1);
    mailspace
        .set_dependencies("ceo", &task, "task", &[], &strings(&[&dep]))
        .unwrap();
    assert!(mailspace.task_depends_on(&delivered_id).unwrap().is_empty());
    assert_eq!(node(&mailspace, &delivered).readiness, "ready");
}

#[test]
fn an_unknown_handle_fails_before_anything_is_written() {
    let (mailspace, _tmp) = roster();
    let (dep, _) = send_item(&mailspace, "need", "work", &[]);
    let (release, _) = send_item(&mailspace, "need", "release", &[]);
    let before = letters(&mailspace);
    for (add, remove) in [
        (vec![dep.clone(), "deadbeef".to_string()], vec![]),
        (vec![dep.clone()], vec!["deadbeef".to_string()]),
    ] {
        let err = mailspace
            .set_dependencies("ceo", &release, "need", &add, &remove)
            .unwrap_err()
            .to_string();
        assert!(err.contains("deadbeef"), "{err}");
    }
    assert_eq!(letters(&mailspace), before);
    assert!(mailspace.backlog_dependencies(&release).unwrap().is_empty());
}

#[test]
fn only_the_author_a_mind_or_an_operator_may_change_dependencies() {
    let (mailspace, _tmp) = roster();
    let (dep, _) = send_item(&mailspace, "need", "work", &[]);
    let (_, delivered) = send_item(&mailspace, "need", "release", &[]);
    let before = letters(&mailspace);
    let err = mailspace
        .set_dependencies("cto", &delivered, "need", &strings(&[&dep]), &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("may not change the dependencies"), "{err}");
    assert_eq!(letters(&mailspace), before);
    assert!(
        mailspace
            .backlog_dependencies(&delivered)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_dependency_already_in_the_asked_state_writes_nothing() {
    let (mailspace, _tmp) = roster();
    let (dep, _) = send_item(&mailspace, "need", "work", &[]);
    let (release, _) = send_item(&mailspace, "need", "release", &[&dep]);
    let before = letters(&mailspace);
    let writes = mailspace
        .set_dependencies("ceo", &release, "need", &strings(&[&dep]), &[])
        .unwrap();
    assert!(!writes[0].changed);
    let (other, _) = send_item(&mailspace, "need", "other", &[]);
    let before_other = letters(&mailspace);
    let writes = mailspace
        .set_dependencies("ceo", &release, "need", &[], &strings(&[&other]))
        .unwrap();
    assert!(!writes[0].changed);
    assert_eq!(before_other, letters(&mailspace));
    assert!(before < before_other);
}

#[test]
fn self_edges_cycles_and_contradictions_are_refused() {
    let (mailspace, _tmp) = roster();
    let (a, _) = send_item(&mailspace, "need", "a", &[]);
    let (b, _) = send_item(&mailspace, "need", "b", &[&a]);
    let before = letters(&mailspace);
    let own = mailspace
        .set_dependencies("ceo", &a, "need", &strings(&[&a]), &[])
        .unwrap_err()
        .to_string();
    assert!(own.contains("cannot depend on itself"), "{own}");
    let cycle = mailspace
        .set_dependencies("ceo", &a, "need", &strings(&[&b]), &[])
        .unwrap_err()
        .to_string();
    assert!(cycle.contains("cycle"), "{cycle}");
    let both = mailspace
        .set_dependencies("ceo", &b, "need", &strings(&[&a]), &strings(&[&a]))
        .unwrap_err()
        .to_string();
    assert!(both.contains("both --add and --remove"), "{both}");
    let none = mailspace
        .set_dependencies("ceo", &b, "need", &[], &[])
        .unwrap_err()
        .to_string();
    assert!(none.contains("at least one"), "{none}");
    let wrong = mailspace
        .set_dependencies("ceo", &b, "task", &strings(&[&a]), &[])
        .unwrap_err()
        .to_string();
    assert!(wrong.contains("is not a task"), "{wrong}");
    assert_eq!(letters(&mailspace), before);
}

#[test]
fn a_mind_that_did_not_author_the_item_may_change_it() {
    let (mailspace, _tmp) = roster();
    let (dep, _) = send_item(&mailspace, "need", "work", &[]);
    let result = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["boss".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "release".into(),
            body: "body".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let held = result.delivered[0].handle.clone();
    mailspace
        .set_dependencies("boss", &held, "need", &strings(&[&dep]), &[])
        .unwrap();
    assert_eq!(node(&mailspace, &held).readiness, "blocked");
}
