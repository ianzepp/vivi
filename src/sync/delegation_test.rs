//! Assigned foreign needs bind and close through their local unit join.

use std::rc::Rc;

use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::BACKLOG_GRAPH_CODE;
use crate::sync::fake_server::{Checkout, FakeServer, checkout};

fn pair() -> (Rc<FakeServer>, Checkout, Checkout, String) {
    let server = FakeServer::new();
    let a = checkout("alice", &["operator", "mind"]);
    let b = checkout("bob", &["mind", "other"]);
    let sent = a
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec![a.address("mind")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "delegated need".into(),
            body: "done_when: its units are complete\n".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
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
            body: "done_when: accepted\n".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
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
    let (server, a, b, need) = pair();
    a.mailspace
        .assign_need("operator", &need, &b.address("mind"))
        .unwrap();
    a.push(&a.client(&server));
    let pulled = b.pull(&b.client(&server));
    assert!(pulled.events_applied > 0, "{}", pulled.summary());

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

    b.push(&b.client(&server));
    a.pull(&a.client(&server));
    assert_eq!(
        a.mailspace.item_event_state(&need).unwrap().unwrap().state,
        "done"
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
    let (server, a, b, need) = pair();
    a.mailspace
        .assign_need("operator", &need, &b.address("mind"))
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));

    a.mailspace
        .move_item("operator", &need, "done", None, "need done", None)
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    a.mailspace
        .move_item("operator", &need, "needs", None, "need reopen", None)
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    assert_eq!(
        b.mailspace
            .assigned_work_for_role(Some("mind"), "needs", "need")
            .unwrap()
            .len(),
        1,
        "an operator reopen returns open work to the active delegate"
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
