//! Assigned foreign needs bind and close through their local unit join.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::boot::BootReport;
use crate::letter::fixture::{need_body, task_body};
use crate::mailspace::{Mailspace, RoleUpdate, SendRequest};
use crate::storage::BACKLOG_GRAPH_CODE;
use crate::sync::fake_server::{Checkout, FakeServer, checkout};

fn pair() -> (Rc<FakeServer>, Checkout, Checkout, String) {
    let server = FakeServer::new();
    let a = checkout("alice", &["operator", "mind", "other"]);
    let b = checkout("bob", &["mind", "other"]);
    let sent = a
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec![a.address("mind")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "delegated need".into(),
            body: need_body(&["its units are complete"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let need = sent.delivered.first().unwrap().handle.clone();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    (server, a, b, need)
}

fn send_unit(mailspace: &Mailspace, role: &str, subject: &str) -> String {
    mailspace
        .send(SendRequest {
            from: role.into(),
            to: vec![mailspace.address_for(role)],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: task_body("accepted\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .first()
        .unwrap()
        .handle
        .clone()
}

#[test]
fn assigned_foreign_need_binds_local_units_and_join_close_travels_home() {
    let (server, mut a, b, need) = pair();
    a.mailspace
        .add_role(
            "worker",
            RoleUpdate {
                kind: Some(Some("hand".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    assert!(
        a.mailspace
            .assign_need("operator", &need, &a.address("worker"))
            .is_err(),
        "a hand-kind role cannot receive a local need assignment"
    );
    let before_assignment = BootReport::collect(&b.mailspace).unwrap();
    assert!(
        before_assignment
            .handles
            .iter()
            .all(|row| row.handle != need),
        "a foreign-held need is absent from boot before assignment"
    );
    a.mailspace
        .assign_need("operator", &need, &b.address("mind"))
        .unwrap();
    let assigned = a.push(&a.client(&server));
    assert!(assigned.sent > 0, "the assignment letter is pushed");
    let pulled = b.pull(&b.client(&server));
    assert!(pulled.events_applied > 0, "{}", pulled.summary());
    let boot = BootReport::collect(&b.mailspace).unwrap();
    assert!(
        boot.handles.iter().any(|row| row.handle == need),
        "the active delegate sees the foreign need in boot: {}",
        boot.render()
    );

    let storage = b.mailspace.storage().unwrap();
    let graph = storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .unwrap();
    assert!(
        storage
            .work_graph_nodes(&graph.handle)
            .unwrap()
            .iter()
            .any(|node| node.source_id == need && node.kind == "need"),
        "the assigned need has a local backlog node"
    );

    let refused = b
        .mailspace
        .move_item("other", &need, "done", None, "need done", None);
    assert!(refused.is_err(), "an unassigned role cannot close the need");
    assert!(
        a.mailspace
            .move_item("other", &need, "done", None, "need done", None)
            .is_err(),
        "an unrelated role in the authoring mailspace cannot close it"
    );

    let first = send_unit(&b.mailspace, "mind", "first unit");
    let second = send_unit(&b.mailspace, "mind", "second unit");
    b.mailspace
        .backlog_bind_units(&need, &[first.clone(), second.clone()])
        .unwrap();
    b.mailspace
        .move_item("mind", &first, "done", None, "task done", None)
        .unwrap();
    b.mailspace
        .move_item("mind", &second, "done", None, "task done", None)
        .unwrap();

    let close_push = b.push(&b.client(&server));
    assert!(close_push.sent > 0, "the join close event is pushed");
    let home_pull = a.pull(&a.client(&server));
    assert!(home_pull.events_applied > 0, "{}", home_pull.summary());
    assert_eq!(
        a.mailspace.item_event_state(&need).unwrap().unwrap().state,
        "done"
    );
    let home_storage = a.mailspace.storage().unwrap();
    assert!(
        home_storage
            .list_messages_by_role("done")
            .unwrap()
            .iter()
            .any(|row| row.handle == need),
        "the author's folder projection moves the joined need to done"
    );
    let home_graph = home_storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .unwrap();
    assert!(
        home_storage
            .work_graph_nodes(&home_graph.handle)
            .unwrap()
            .iter()
            .any(|node| node.source_id == need && node.state == "done"),
        "the author's backlog node reaches the graph's done state with the joined need"
    );
    assert!(
        BootReport::collect(&a.mailspace)
            .unwrap()
            .handles
            .iter()
            .all(|row| row.handle != need),
        "the completed need leaves the author's open boot inventory"
    );

    a.mailspace
        .move_item("operator", &need, "needs", None, "need reopen", None)
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    assert!(
        b.mailspace
            .move_item("mind", &need, "done", None, "need done", None)
            .is_err(),
        "the delegate's close lapses its authority"
    );
    a.mailspace
        .assign_need("operator", &need, &b.address("other"))
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    b.mailspace
        .move_item("other", &need, "done", None, "need done", None)
        .unwrap();
}

#[test]
fn withdraw_and_newer_assignment_remove_old_delegate_authority() {
    let (server, mut a, b, need) = pair();
    a.mailspace
        .add_role(
            "release-ops",
            RoleUpdate {
                kind: Some(Some("operator".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    a.mailspace
        .assign_need("operator", &need, &b.address("mind"))
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));

    a.mailspace
        .move_item("release-ops", &need, "done", None, "need done", None)
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    b.mailspace
        .move_item("mind", &need, "needs", None, "need reopen", None)
        .unwrap();
    b.push(&b.client(&server));
    a.pull(&a.client(&server));
    assert_eq!(
        a.mailspace.item_event_state(&need).unwrap().unwrap().state,
        "reopened",
        "the active delegate can reopen a need closed by the author"
    );
    assert!(
        b.mailspace
            .storage()
            .unwrap()
            .list_messages_by_role("needs")
            .unwrap()
            .iter()
            .any(|row| row.handle == need),
        "the delegate's reopen returns the work to its needs folder"
    );

    a.mailspace
        .move_item("release-ops", &need, "done", None, "need done", None)
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    a.mailspace
        .move_item("release-ops", &need, "needs", None, "need reopen", None)
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    let boot = BootReport::collect(&b.mailspace).unwrap();
    assert!(
        boot.handles.iter().any(|row| row.handle == need),
        "an operator reopen returns open work to the active delegate's boot: {}",
        boot.render()
    );
    let delegate_storage = b.mailspace.storage().unwrap();
    let delegate_graph = delegate_storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .unwrap();
    assert!(
        delegate_storage
            .work_graph_nodes(&delegate_graph.handle)
            .unwrap()
            .iter()
            .any(|node| node.source_id == need && node.state == "open"),
        "the delegate's backlog node reopens with the operator event"
    );

    a.mailspace.withdraw_need("operator", &need).unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    assert!(
        b.mailspace
            .move_item("mind", &need, "done", None, "need done", None)
            .is_err(),
        "withdrawal revokes the former delegate"
    );

    a.mailspace
        .assign_need("operator", &need, &b.address("other"))
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    assert!(
        b.mailspace
            .move_item("mind", &need, "done", None, "need done", None)
            .is_err(),
        "the newer assignment replaces the former delegate"
    );
    b.mailspace
        .move_item("other", &need, "done", None, "need done", None)
        .unwrap();
}

#[test]
fn assigned_delivery_need_appears_in_merge_boot_and_close_syncs_home() {
    let (server, a, mut b, _) = pair();
    b.mailspace
        .add_role(
            "merge",
            RoleUpdate {
                kind: Some(Some("merge".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    let delivery = a
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![a.address("mind")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Delivery: mind/review".into(),
            body: need_body(
                &["branch mind/review at deadbeef1 is merged into cafebabe2"],
                r#"{"schema":1,"branch":"mind/review","local_branch":"review","tip":"deadbeef1","base":"cafebabe2","remote":"origin","push":{"status":"pushed","remote_ref":"refs/heads/mind/review","detail":null},"gate":{"command":"cargo test","result":"passed"},"closes":[],"blocked":[],"filed":[]}"#,
            ),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .first()
        .unwrap()
        .handle
        .clone();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    a.mailspace
        .assign_need("mind", &delivery, &b.address("merge"))
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));

    let boot = BootReport::collect(&b.mailspace).unwrap();
    assert!(
        boot.handles
            .iter()
            .any(|row| row.handle == delivery && row.subject == "Delivery: mind/review"),
        "the merge role's assigned delivery must appear in boot: {}",
        boot.render()
    );

    let metadata = BTreeMap::from([
        ("branch".into(), "main".into()),
        ("commit".into(), "deadbeef1234".into()),
    ]);
    let closed = b
        .mailspace
        .move_item_outcome(
            "merge",
            &delivery,
            "done",
            Some("merged as deadbeef1234"),
            "need done",
            Some(&metadata),
        )
        .unwrap();
    assert!(
        closed.event.is_some(),
        "the delegate close travels as an event"
    );
    b.push(&b.client(&server));
    a.pull(&a.client(&server));

    let thread = a.mailspace.thread(&delivery, false, 20, 20).unwrap();
    let need = thread
        .iter()
        .find(|message| message.handle == delivery)
        .unwrap();
    assert_eq!(need.close_branch.as_deref(), Some("main"));
    assert_eq!(need.close_commit.as_deref(), Some("deadbeef1234"));
}
