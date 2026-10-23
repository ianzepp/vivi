use std::collections::BTreeMap;

use crate::letter::fixture::{body_with_deps, need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{Mailspace, SendRequest, WantState};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

fn send(mailspace: &Mailspace, kind: &str, body: String) -> String {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("{kind} subject"),
            body,
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap();
    result.delivered[0].handle.clone()
}

fn need(mailspace: &Mailspace, serves: Option<&str>, explores: Option<&str>) -> String {
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = serves.map(str::to_string);
    front.explores = explores.map(str::to_string);
    send(mailspace, "need", need_body_with(&front, "prose"))
}

fn close(mailspace: &Mailspace, handle: &str, verdict: Option<&str>) {
    let metadata = verdict.map(|v| BTreeMap::from([("verdict".to_string(), v.to_string())]));
    mailspace
        .move_item("mind", handle, "done", None, "close", metadata.as_ref())
        .unwrap();
}

fn reopen(mailspace: &Mailspace, handle: &str) {
    mailspace
        .move_item("mind", handle, "needs", None, "reopen", None)
        .unwrap();
}

/// The state of the node of kind `kind` and whether it is ready.
fn node(mailspace: &Mailspace, kind: &str) -> (String, bool) {
    let show = mailspace.graph_show("backlog").unwrap();
    let node = show.nodes.iter().find(|n| n.kind == kind).unwrap();
    let ready = show.ready.iter().any(|n| n.handle == node.handle);
    (node.state.clone(), ready)
}

fn want_done(mailspace: &Mailspace) -> bool {
    node(mailspace, "want").0 == "done"
}

#[test]
fn the_want_node_follows_its_serving_needs() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p"));
    let task = send(
        &mailspace,
        "task",
        body_with_deps("task", "p", std::slice::from_ref(&want)),
    );
    let a = need(&mailspace, Some(&want), None);
    assert_eq!(node(&mailspace, "want").0, "open");
    assert_eq!(node(&mailspace, "task"), ("open".into(), false), "{task}");

    close(&mailspace, &a, None);
    assert_eq!(node(&mailspace, "want").0, "done");
    assert_eq!(node(&mailspace, "task"), ("open".into(), true));
    assert_eq!(mailspace.want_state(&want).unwrap().state, WantState::Done);

    // A new serving need reopens the node with no action on the want.
    let b = need(&mailspace, Some(&want), None);
    assert_eq!(node(&mailspace, "want").0, "open");
    assert!(!node(&mailspace, "task").1);

    close(&mailspace, &b, None);
    assert!(want_done(&mailspace));
    assert!(node(&mailspace, "task").1);

    // Reopening a serving need reopens the node too.
    reopen(&mailspace, &a);
    assert!(!want_done(&mailspace));
    assert!(!node(&mailspace, "task").1);
    close(&mailspace, &a, None);
    assert!(want_done(&mailspace));
}

#[test]
fn an_exploring_need_changes_nothing() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p"));
    let served = need(&mailspace, Some(&want), None);
    close(&mailspace, &served, None);
    assert!(want_done(&mailspace));

    let explorer = need(&mailspace, None, Some(&want));
    assert!(want_done(&mailspace));
    close(&mailspace, &explorer, None);
    assert!(want_done(&mailspace));
}

#[test]
fn a_withdrawn_serving_need_counts() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p"));
    let a = need(&mailspace, Some(&want), None);
    close(&mailspace, &a, Some("withdrawn"));
    assert!(want_done(&mailspace));
}

#[test]
fn a_want_attached_after_its_serving_need_closed_settles() {
    let (_tmp, mailspace) = space();
    let want = send(&mailspace, "want", want_body("q", "p"));
    let task = send(
        &mailspace,
        "task",
        body_with_deps("task", "p", std::slice::from_ref(&want)),
    );
    let served = need(&mailspace, Some(&want), None);
    close(&mailspace, &served, None);
    assert!(want_done(&mailspace));

    // The want's node is minted open when its letter arrives after the
    // need's close (a later recipient, a restore): put it back to that state.
    let (row, bytes, subject) = {
        let storage = mailspace.storage().unwrap();
        let graph = storage.work_graph_by_code("backlog").unwrap().unwrap();
        let node = storage
            .work_graph_nodes(&graph.handle)
            .unwrap()
            .into_iter()
            .find(|n| n.kind == "want")
            .unwrap();
        storage
            .set_work_graph_node_state(&graph.handle, &node.handle, "open", None)
            .unwrap();
        let row = storage.resolve_message_token(&want).unwrap();
        let bytes = storage.read_message(&row).unwrap();
        (row, bytes, node.label)
    };
    assert!(!want_done(&mailspace));
    assert!(!node(&mailspace, "task").1, "{task}");

    mailspace
        .backlog_attach_letter("want", &row, &subject, &bytes)
        .unwrap();
    assert!(want_done(&mailspace));
    assert!(node(&mailspace, "task").1);
}
