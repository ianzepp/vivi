use super::*;
use crate::letter::fixture::{body_with_deps, need_body};
use crate::mailspace::{GraphNodeView, SendRequest};
use crate::storage::Storage;

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    (mailspace, tmp)
}

fn send_item(
    mailspace: &Mailspace,
    role: &str,
    kind: Option<&str>,
    subject: &str,
    depends_on: Vec<String>,
) -> String {
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: body_with_deps(kind.unwrap_or("mail"), "body", &depends_on),
            role: role.into(),
            kind: kind.map(str::to_string),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn node(mailspace: &Mailspace, source_id: &str) -> GraphNodeView {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.source_id == source_id)
        .unwrap()
}

#[test]
fn attach_mints_backlog_node_ready() {
    let (mailspace, _tmp) = roster();
    mailspace
        .backlog_attach("want", "w1handle0", "extract retry helper", &[])
        .unwrap();
    let view = node(&mailspace, "w1handle0");
    assert_eq!(view.state, "open");
    assert_eq!(view.readiness, "ready");
    assert_eq!(view.label, "extract retry helper");
}

#[test]
fn attach_is_idempotent() {
    let (mailspace, _tmp) = roster();
    mailspace
        .backlog_attach("need", "n1handle0", "fix auth", &[])
        .unwrap();
    mailspace
        .backlog_attach("need", "n1handle0", "fix auth", &[])
        .unwrap();
    assert_eq!(mailspace.graph_show("backlog").unwrap().nodes.len(), 1);
}

#[test]
fn dependency_blocks_until_completed() {
    let (mailspace, _tmp) = roster();
    let dep = send_item(&mailspace, "needs", Some("need"), "auth fix", Vec::new());
    mailspace
        .backlog_attach("need", &dep, "auth fix", &[])
        .unwrap();

    mailspace
        .backlog_attach("want", "w2handle0", "retry helper", &[dep.clone()])
        .unwrap();
    assert_eq!(node(&mailspace, "w2handle0").readiness, "blocked");
    assert_eq!(node(&mailspace, &dep).readiness, "ready");

    mailspace.backlog_complete_item(&dep).unwrap();
    assert_eq!(node(&mailspace, &dep).state, "done");
    assert_eq!(node(&mailspace, "w2handle0").readiness, "ready");
}

#[test]
fn done_dependency_mints_done_node_and_unlocks() {
    let (mailspace, _tmp) = roster();
    let dep = send_item(&mailspace, "needs", Some("need"), "old fix", Vec::new());
    mailspace
        .move_item("ceo", &dep, "done", None, "need done", None)
        .unwrap();

    mailspace
        .backlog_attach("want", "w3handle0", "follow-up", &[dep])
        .unwrap();
    assert_eq!(node(&mailspace, "w3handle0").readiness, "ready");
}

#[test]
fn validate_deps_rejects_unknown_handle() {
    let (mailspace, _tmp) = roster();
    let err = mailspace
        .backlog_validate_deps(&["deadbeef".into()])
        .unwrap_err();
    assert!(err.to_string().contains("deadbeef"), "{err}");
}

#[test]
fn validate_deps_accepts_task_handle() {
    let (mailspace, _tmp) = roster();
    let task = send_item(&mailspace, "tasks", Some("task"), "do work", Vec::new());
    mailspace.backlog_validate_deps(&[task]).unwrap();
}

#[test]
fn task_send_mints_node_and_task_dep_unlocks() {
    let (mailspace, _tmp) = roster();
    let first = send_item(&mailspace, "tasks", Some("task"), "unit A", Vec::new());
    let second = send_item(
        &mailspace,
        "tasks",
        Some("task"),
        "unit B",
        vec![first.clone()],
    );

    assert_eq!(node(&mailspace, &first).readiness, "ready");
    assert_eq!(node(&mailspace, &second).readiness, "blocked");

    mailspace
        .move_item("ceo", &first, "done", None, "task done", None)
        .unwrap();
    assert_eq!(node(&mailspace, &first).state, "done");
    assert_eq!(node(&mailspace, &second).readiness, "ready");

    mailspace
        .move_item("ceo", &first, "tasks", None, "task reopen", None)
        .unwrap();
    assert_eq!(node(&mailspace, &first).state, "open");
    assert_eq!(node(&mailspace, &second).readiness, "blocked");
}

#[test]
fn task_depends_on_need_unlocks_cross_kind() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "auth fix", Vec::new());
    let task = send_item(
        &mailspace,
        "tasks",
        Some("task"),
        "audit wave",
        vec![need.clone()],
    );

    assert_eq!(node(&mailspace, &task).readiness, "blocked");
    mailspace.backlog_complete_item(&need).unwrap();
    assert_eq!(node(&mailspace, &task).readiness, "ready");
}

#[test]
fn send_rejects_unknown_dep_before_creating_anything() {
    let (mailspace, _tmp) = roster();
    let before = mailspace.list_kind(None, "tasks", "task").unwrap().len();
    let err = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "bad dep".into(),
            body: body_with_deps("task", "body", &["deadbeef".to_string()]),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap_err();
    assert!(err.to_string().contains("deadbeef"), "{err}");
    let after = mailspace.list_kind(None, "tasks", "task").unwrap().len();
    assert_eq!(before, after);
}

#[test]
fn lifecycle_moves_sync_node_state() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "fix auth", Vec::new());
    mailspace
        .backlog_attach("need", &need, "fix auth", &[])
        .unwrap();

    mailspace
        .move_item("ceo", &need, "done", None, "need done", None)
        .unwrap();
    assert_eq!(node(&mailspace, &need).state, "done");

    mailspace
        .move_item("ceo", &need, "needs", None, "need reopen", None)
        .unwrap();
    assert_eq!(node(&mailspace, &need).state, "open");
}

#[test]
fn promotion_leaves_node_open() {
    let (mailspace, _tmp) = roster();
    let want = send_item(&mailspace, "wants", Some("want"), "later work", Vec::new());
    mailspace
        .backlog_attach("want", &want, "later work", &[])
        .unwrap();

    mailspace
        .move_item("cto", &want, "needs", None, "want promote", None)
        .unwrap();
    assert_eq!(node(&mailspace, &want).state, "open");
    assert_eq!(node(&mailspace, &want).readiness, "ready");
}

#[test]
fn complete_without_graph_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.backlog_complete_item("missing0").unwrap();
    mailspace.backlog_reopen_item("missing0").unwrap();
    assert!(
        mailspace
            .storage()
            .unwrap()
            .work_graphs()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mint_records_backlog_events() {
    let (mailspace, _tmp) = roster();
    mailspace
        .backlog_attach("need", "n9handle0", "audit", &[])
        .unwrap();
    let storage = mailspace.storage().unwrap();
    let events = storage.list_work_graph_events_after(0).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.event_type == "backlog_attached" && e.note.as_deref() == Some("kind=need")),
        "{events:?}"
    );
}

#[test]
fn bind_joins_need_completion_on_last_unit() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "lower me", Vec::new());
    let first = send_item(&mailspace, "tasks", Some("task"), "unit 1", Vec::new());
    let second = send_item(&mailspace, "tasks", Some("task"), "unit 2", Vec::new());
    let follower = send_item(
        &mailspace,
        "wants",
        Some("want"),
        "follow",
        vec![need.clone()],
    );
    mailspace
        .backlog_bind_units(&need, &[first.clone(), second.clone()])
        .unwrap();

    assert_eq!(node(&mailspace, &follower).readiness, "blocked");

    mailspace.backlog_complete_item(&first).unwrap();
    assert_eq!(node(&mailspace, &need).state, "open");
    assert_eq!(node(&mailspace, &follower).readiness, "blocked");

    mailspace.backlog_complete_item(&second).unwrap();
    assert_eq!(node(&mailspace, &need).state, "done");
    assert_eq!(node(&mailspace, &follower).readiness, "ready");
}

#[test]
fn joined_need_uses_common_unit_branch_and_latest_unit_commit() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "lower me", Vec::new());
    let first = send_item(&mailspace, "tasks", Some("task"), "unit 1", Vec::new());
    let second = send_item(&mailspace, "tasks", Some("task"), "unit 2", Vec::new());
    mailspace
        .backlog_bind_units(&need, &[first.clone(), second.clone()])
        .unwrap();

    for (unit, commit) in [(&first, "1111111"), (&second, "2222222")] {
        let metadata = BTreeMap::from([
            ("branch".into(), "mind/review".into()),
            ("commit".into(), commit.into()),
        ]);
        mailspace
            .move_item_outcome("ceo", unit, "done", None, "task done", Some(&metadata))
            .unwrap();
    }

    let metadata = mailspace.storage().unwrap().item_metadata(&need).unwrap();
    assert_eq!(
        metadata.get("branch").map(String::as_str),
        Some("mind/review")
    );
    assert_eq!(metadata.get("commit").map(String::as_str), Some("2222222"));
    let state = mailspace.item_event_state(&need).unwrap().unwrap();
    let storage = mailspace.storage().unwrap();
    let event = storage
        .live_message_by_rfc_id(&state.event)
        .unwrap()
        .unwrap();
    let bytes = storage.read_message(&event.message_id).unwrap();
    let letter = String::from_utf8_lossy(&bytes);
    assert!(letter.contains("X-Vivi-Branch: mind/review"), "{letter}");
    assert!(letter.contains("X-Vivi-Commit: 2222222"), "{letter}");
}

#[test]
fn joined_need_omits_provenance_when_units_name_different_branches() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "lower me", Vec::new());
    let first = send_item(&mailspace, "tasks", Some("task"), "unit 1", Vec::new());
    let second = send_item(&mailspace, "tasks", Some("task"), "unit 2", Vec::new());
    mailspace
        .backlog_bind_units(&need, &[first.clone(), second.clone()])
        .unwrap();

    for (unit, branch, commit) in [
        (&first, "mind/a", "1111111"),
        (&second, "mind/b", "2222222"),
    ] {
        let metadata = BTreeMap::from([
            ("branch".into(), branch.into()),
            ("commit".into(), commit.into()),
        ]);
        mailspace
            .move_item_outcome("ceo", unit, "done", None, "task done", Some(&metadata))
            .unwrap();
    }

    let metadata = mailspace.storage().unwrap().item_metadata(&need).unwrap();
    assert!(!metadata.contains_key("branch"), "{metadata:?}");
    assert!(!metadata.contains_key("commit"), "{metadata:?}");
    assert_eq!(node(&mailspace, &need).state, "done");
}

#[test]
fn retro_bind_of_done_units_completes_need() {
    let (mailspace, _tmp) = roster();
    let need = send_item(
        &mailspace,
        "needs",
        Some("need"),
        "late lowering",
        Vec::new(),
    );
    let unit = send_item(
        &mailspace,
        "tasks",
        Some("task"),
        "already landed",
        Vec::new(),
    );
    mailspace.backlog_complete_item(&unit).unwrap();

    mailspace.backlog_bind_units(&need, &[unit]).unwrap();
    assert_eq!(node(&mailspace, &need).state, "done");
}

#[test]
fn bind_rejects_missing_unit_node() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "bind me", Vec::new());
    let err = mailspace
        .backlog_bind_units(&need, &["unsent00".into()])
        .unwrap_err();
    assert!(err.to_string().contains("no graph node"), "{err}");
}

#[test]
fn bind_rejects_done_parent() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "closed", Vec::new());
    let unit = send_item(&mailspace, "tasks", Some("task"), "unit", Vec::new());
    mailspace.backlog_complete_item(&need).unwrap();
    let err = mailspace.backlog_bind_units(&need, &[unit]).unwrap_err();
    assert!(err.to_string().contains("state"), "{err}");
}

#[test]
fn bind_rejects_task_parent() {
    let (mailspace, _tmp) = roster();
    let task = send_item(&mailspace, "tasks", Some("task"), "not a need", Vec::new());
    let unit = send_item(&mailspace, "tasks", Some("task"), "unit", Vec::new());
    let err = mailspace.backlog_bind_units(&task, &[unit]).unwrap_err();
    assert!(err.to_string().contains("onto needs"), "{err}");
}

fn role_of(mailspace: &Mailspace, handle: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let message_id = storage.resolve_message_token(handle).unwrap();
    storage
        .message_by_id(&message_id)
        .unwrap()
        .expect("message exists")
        .local_role
}

#[test]
fn join_settles_need_mailbox_and_reopen_restores() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "lower me", Vec::new());
    let unit = send_item(&mailspace, "tasks", Some("task"), "unit", Vec::new());
    mailspace
        .backlog_bind_units(&need, &[unit.clone()])
        .unwrap();
    assert_eq!(role_of(&mailspace, &need), "needs");

    // The author accepting the unit completes it (an assignee's `done` is
    // only a report); the join must settle the need's mailbox copy too, not
    // only its graph node.
    mailspace
        .move_item("ceo", &unit, "done", None, "task done", None)
        .unwrap();
    assert_eq!(node(&mailspace, &need).state, "done");
    assert_eq!(
        role_of(&mailspace, &need),
        "done",
        "join must close the need item"
    );

    // Reopening the unit invalidates the join: node AND mailbox re-open.
    mailspace
        .move_item("ceo", &unit, "tasks", None, "task reopen", None)
        .unwrap();
    assert_eq!(node(&mailspace, &need).state, "open");
    assert_eq!(role_of(&mailspace, &need), "needs");
}

#[test]
fn join_settle_moves_every_recipient_copy() {
    let (mut mailspace, _tmp) = roster();
    mailspace.add_identity("coo").unwrap();
    let result = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into(), "coo".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "shared need".into(),
            body: need_body(&["exists"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let copies: Vec<String> = result.delivered.iter().map(|d| d.handle.clone()).collect();
    let unit = send_item(&mailspace, "tasks", Some("task"), "unit", Vec::new());
    mailspace
        .backlog_bind_units(&copies[0], &[unit.clone()])
        .unwrap();

    mailspace
        .move_item("ceo", &unit, "done", None, "task done", None)
        .unwrap();
    for copy in &copies {
        assert_eq!(role_of(&mailspace, copy), "done", "copy {copy} settled");
    }
}

#[test]
fn reopen_cascades_to_done_join_parent() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", Some("need"), "join me", Vec::new());
    let first = send_item(&mailspace, "tasks", Some("task"), "unit 1", Vec::new());
    let second = send_item(&mailspace, "tasks", Some("task"), "unit 2", Vec::new());
    mailspace
        .backlog_bind_units(&need, &[first.clone(), second.clone()])
        .unwrap();
    mailspace.backlog_complete_item(&first).unwrap();
    mailspace.backlog_complete_item(&second).unwrap();
    assert_eq!(node(&mailspace, &need).state, "done");

    // Reopening one unit invalidates the join: the done parent re-opens.
    mailspace.backlog_reopen_item(&second).unwrap();
    assert_eq!(node(&mailspace, &second).state, "open");
    assert_eq!(node(&mailspace, &need).state, "open");
    // The untouched sibling keeps its state.
    assert_eq!(node(&mailspace, &first).state, "done");

    // Re-completing the reopened unit closes the join again.
    mailspace.backlog_complete_item(&second).unwrap();
    assert_eq!(node(&mailspace, &need).state, "done");
}

#[test]
fn dependency_edges_canonicalize_across_copies() {
    let (mailspace, _tmp) = roster();
    // One need, two recipients: two delivered copies of the same content.
    let result = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into(), "ceo".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "shared dep".into(),
            body: need_body(&["exists"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let copies: Vec<String> = result.delivered.iter().map(|d| d.handle.clone()).collect();
    assert_eq!(copies.len(), 2, "two delivered copies");

    // Dependents citing different copies share one canonical edge source.
    mailspace
        .backlog_attach("want", "wcop0001", "cites copy 1", &[copies[0].clone()])
        .unwrap();
    mailspace
        .backlog_attach("want", "wcop0002", "cites copy 2", &[copies[1].clone()])
        .unwrap();
    let show = mailspace.graph_show("backlog").unwrap();
    let sources: std::collections::BTreeSet<String> = show
        .edges
        .iter()
        .map(|e| e.from_source_id.clone())
        .collect();
    assert_eq!(sources.len(), 1, "one canonical dep node: {sources:?}");
}

#[test]
fn completing_any_copy_unlocks_dependents_of_the_other() {
    let (mailspace, _tmp) = roster();
    let result = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into(), "ceo".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "shared dep".into(),
            body: need_body(&["exists"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let copies: Vec<String> = result.delivered.iter().map(|d| d.handle.clone()).collect();

    // The dependent cites copy 2, but copy 1 is the one that gets done.
    mailspace
        .backlog_attach("want", "wany0001", "follow", &[copies[1].clone()])
        .unwrap();
    assert_eq!(node(&mailspace, "wany0001").readiness, "blocked");

    mailspace.backlog_complete_item(&copies[0]).unwrap();
    assert_eq!(
        node(&mailspace, "wany0001").readiness,
        "ready",
        "sibling completion must propagate"
    );
    // The sibling copy's own node completed too.
    assert_eq!(node(&mailspace, &copies[1]).state, "done");
}
