use super::*;
use crate::mailspace::{RoleUpdate, SendRequest};

fn roster() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    (mailspace, tmp)
}

fn send_item(mailspace: &Mailspace, role: &str, kind: &str, subject: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "done_when: done".into(),
            role: role.into(),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn deliver_untracked_task(mailspace: &Mailspace, subject: &str) {
    // An .eml delivered straight into the tasks folder never runs the send
    // path — the shape an older vivi binary (no node minting) leaves behind.
    let eml = format!(
        "From: {}\r\nTo: {}\r\nSubject: {subject}\r\nX-Vivi-Kind: task\r\n\r\nplain body\r\n",
        mailspace.address_for("ceo"),
        mailspace.address_for("cto"),
    );
    mailspace.deliver_raw(eml.as_bytes(), "tasks").unwrap();
}

#[test]
fn audit_is_clean_on_a_healthy_board() {
    let (mailspace, _tmp) = roster();
    send_item(&mailspace, "tasks", "task", "healthy");
    let report = mailspace.backlog_audit(false).unwrap();
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    assert_eq!(report.checked, 1);
}

#[test]
fn audit_flags_and_repairs_missing_node() {
    let (mailspace, _tmp) = roster();
    send_item(&mailspace, "tasks", "task", "tracked");
    deliver_untracked_task(&mailspace, "untracked");

    let report = mailspace.backlog_audit(false).unwrap();
    assert_eq!(report.findings.len(), 1, "{:?}", report.findings);
    assert_eq!(report.findings[0].class, "missing_node");
    assert_eq!(report.findings[0].kind, "task");
    assert!(!report.findings[0].repaired);

    let repaired = mailspace.backlog_audit(true).unwrap();
    assert!(repaired.findings[0].repaired, "{:?}", repaired.findings);

    // After repair the item is addressable and the board is clean again.
    let after = mailspace.backlog_audit(false).unwrap();
    assert!(after.findings.is_empty(), "{:?}", after.findings);
    let tracked = mailspace
        .backlog_item_tracked(&repaired.findings[0].handle)
        .unwrap();
    assert!(tracked);
}

#[test]
fn audit_repair_mints_done_items_as_done_nodes() {
    let (mailspace, _tmp) = roster();
    send_item(&mailspace, "tasks", "task", "tracked");
    deliver_untracked_task(&mailspace, "settled untracked");

    // The untracked item is settled by its holder without any node.
    let untracked = mailspace
        .list_kind(Some("cto"), "tasks", "task")
        .unwrap()
        .into_iter()
        .find(|view| view.subject == "settled untracked")
        .unwrap();
    mailspace
        .storage()
        .unwrap()
        .move_message_to_role(&untracked.account, &untracked.message_id, "done")
        .unwrap();

    mailspace.backlog_audit(true).unwrap();
    let show = mailspace.graph_show("backlog").unwrap();
    let node = show
        .nodes
        .iter()
        .find(|n| n.label == "settled untracked")
        .unwrap();
    assert_eq!(node.state, "done", "repaired node must match the folder");
}

#[test]
fn audit_flags_and_repairs_state_drift() {
    let (mut mailspace, _tmp) = roster();
    // A mind's `done` is an acceptance, so a mind's done folder under an open
    // node cannot be a report: it is drift.
    mailspace
        .set_role(
            "cto",
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let task = send_item(&mailspace, "tasks", "task", "drifts");

    // Move the message behind the graph's back: folder done, node open.
    let view = mailspace
        .storage()
        .unwrap()
        .message_by_id(
            &mailspace
                .storage()
                .unwrap()
                .resolve_message_token(&task)
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    mailspace
        .storage()
        .unwrap()
        .move_message_to_role(&view.account, &view.message_id, "done")
        .unwrap();

    let report = mailspace.backlog_audit(false).unwrap();
    assert_eq!(report.findings.len(), 1, "{:?}", report.findings);
    assert_eq!(report.findings[0].class, "node_state");

    mailspace.backlog_audit(true).unwrap();
    let show = mailspace.graph_show("backlog").unwrap();
    let node = show.nodes.iter().find(|n| n.source_id == task).unwrap();
    assert_eq!(node.state, "done");
}

#[test]
fn audit_repair_settles_need_mailbox_when_join_fired_without_settle() {
    let (mailspace, _tmp) = roster();
    let need = send_item(&mailspace, "needs", "need", "pre-fix join");
    let unit = send_item(&mailspace, "tasks", "task", "unit");
    mailspace
        .backlog_bind_units(&need, &[unit.clone()])
        .unwrap();
    // Settle the unit through the lifecycle, then undo only the need's
    // mailbox settle — the exact state pre-fix joins left behind: need node
    // done via the join, need item still in the needs folder.
    mailspace
        .move_item("ceo", &unit, "done", None, "task done", None)
        .unwrap();
    let storage = mailspace.storage().unwrap();
    let need_id = storage.resolve_message_token(&need).unwrap();
    let view = storage.message_by_id(&need_id).unwrap().unwrap();
    assert_eq!(view.local_role, "done", "join settle must close the need");
    storage
        .move_message_to_role(&view.account, &need_id, "needs")
        .unwrap();
    // A pre-fix join wrote no event letter: forget the one this join wrote,
    // so the repair has the node and the folder to go by, as it did then.
    rusqlite::Connection::open(mailspace.store_path())
        .unwrap()
        .execute("DELETE FROM mailspace_links", [])
        .unwrap();

    let report = mailspace.backlog_audit(true).unwrap();
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.class == "node_state" && f.repaired),
        "{:?}",
        report.findings
    );
    let storage = mailspace.storage().unwrap();
    let role = storage
        .message_by_id(&storage.resolve_message_token(&need).unwrap())
        .unwrap()
        .unwrap()
        .local_role;
    assert_eq!(role, "done", "repair must settle the need's mailbox");
}

#[test]
fn audit_flags_orphan_node_for_deleted_item() {
    let (mailspace, _tmp) = roster();
    let want = send_item(&mailspace, "wants", "want", "doomed");
    let storage = mailspace.storage().unwrap();
    let message_id = storage.resolve_message_token(&want).unwrap();
    let view = storage.message_by_id(&message_id).unwrap().unwrap();
    storage
        .mark_message_deleted(&view.account, &message_id)
        .unwrap();

    let report = mailspace.backlog_audit(false).unwrap();
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.class == "orphan_node" && f.handle == want),
        "{:?}",
        report.findings
    );
}
