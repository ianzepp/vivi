//! A want's state follows its serving needs across machines: the owner closes
//! the need, and the assignee's pulled copy of the want is done with its node
//! completed, with no letter about the want itself.

use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{AbsorbConfig, SendRequest, WantState};
use crate::sync::fake_server::{Checkout, FakeServer, checkout};

fn send(from: &Checkout, to: &str, kind: &str, body: String) -> String {
    let result = from
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("{kind} subject"),
            body,
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap();
    result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone())
}

fn want_node_state(checkout: &Checkout) -> String {
    checkout
        .mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.kind == "want")
        .expect("a want node")
        .state
}

#[test]
fn a_pulled_close_of_the_serving_need_completes_the_want_node() {
    let server = FakeServer::new();
    let mut a = checkout("alice", &["mind"]);
    let b = checkout("bob", &["mind", "hand"]);
    a.mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
    let hand = b.address("hand");

    let want = send(&a, &hand, "want", want_body("q", "p"));
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want.clone());
    let need = send(&a, &hand, "need", need_body_with(&front, "p"));
    a.push(&a.client(&server));
    b.pull(&b.client(&server));

    assert_eq!(
        b.mailspace.want_state(&want).unwrap().state,
        WantState::Open
    );
    assert_eq!(want_node_state(&b), "open");
    assert_eq!(want_node_state(&a), "open");

    a.mailspace
        .move_item("mind", &need, "done", None, "close", None)
        .unwrap();
    assert_eq!(want_node_state(&a), "done");
    a.push(&a.client(&server));
    b.pull(&b.client(&server));

    let report = b.mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Done, "{report:?}");
    assert_eq!(want_node_state(&b), "done");
}

#[test]
fn a_pulled_hand_close_of_a_want_with_an_open_serving_need_leaves_the_node_open() {
    let server = FakeServer::new();
    let a = checkout("alice", &["mind"]);
    let b = checkout("bob", &["mind", "hand"]);
    let hand = b.address("hand");

    // Alice holds the want and no serving need, so she may close it by hand.
    let want_sent = send(&a, &hand, "want", want_body("q", "p"));
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    let want = want_sent.clone();

    // Bob serves the want with a need that stays open.
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want.clone());
    let need = send(&b, &hand, "need", need_body_with(&front, "p"));
    assert_eq!(want_node_state(&b), "open");

    a.mailspace
        .move_item("mind", &want_sent, "done", None, "close", None)
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));

    let report = b.mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Open, "{report:?}");
    assert_eq!(want_node_state(&b), "open");

    // The need's close then settles it.
    b.mailspace
        .move_item("mind", &need, "done", None, "close", None)
        .unwrap();
    assert_eq!(want_node_state(&b), "done");
}
