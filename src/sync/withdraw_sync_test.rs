//! An operator withdraw travels: `vivi pump withdraw` on the pump member
//! closes the need `withdrawn` as an event letter, and a member that pulls it
//! reads the need closed, the want it serves done, and the want's node done.

use std::fs;

use crate::cli::PumpWithdrawCommand;
use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{SendRequest, WantState, held_item, is_done};
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::withdraw::run;
use crate::sync::fake_server::{Checkout, FakeServer, checkout};

fn send(from: &Checkout, to: &str, kind: &str, body: String) -> String {
    from.mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec![from.address("mind"), to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("{kind} subject"),
            body,
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
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

fn need_verdict(checkout: &Checkout, need: &str) -> (bool, Option<String>) {
    let storage = checkout.mailspace.storage().unwrap();
    let item = held_item(&storage, need).unwrap();
    let verdict = item.rows.iter().find_map(|row| {
        storage
            .item_metadata(&row.message_id)
            .unwrap()
            .get("verdict")
            .filter(|v| !v.is_empty())
            .cloned()
    });
    (is_done(&item), verdict)
}

#[test]
fn a_withdraw_on_the_pump_member_closes_the_need_and_the_want_on_another_member() {
    let server = FakeServer::new();
    let a = checkout("alice", &["operator", "mind"]);
    let b = checkout("bob", &["mind", "hand"]);
    let hand = b.address("hand");

    let toml = a.mailspace.root.join(".vivi").join("mailspace.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[pump]\nbranch = \"next\"\n");
    fs::write(&toml, text).unwrap();

    let want = send(&a, &hand, "want", want_body("q", "p"));
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want.clone());
    let need = send(&a, &hand, "need", need_body_with(&front, "p"));
    a.mailspace.pump_send(None, &need, None).unwrap();
    let home = a.mailspace.root.join(".vivi").join("pump");
    fs::create_dir_all(&home).unwrap();
    PumpStore::open(&home.join(DB_FILENAME))
        .unwrap()
        .save(&State::default())
        .unwrap();
    a.push(&a.client(&server));
    b.pull(&b.client(&server));

    assert_eq!(
        b.mailspace.want_state(&want).unwrap().state,
        WantState::Open
    );
    assert_eq!(want_node_state(&b), "open");
    assert!(!need_verdict(&b, &need).0);

    run(
        &a.mailspace.root,
        &PumpWithdrawCommand {
            needs: vec![need.clone()],
            reason: "stale".into(),
            project: None,
        },
    )
    .unwrap();
    assert_eq!(need_verdict(&a, &need), (true, Some("withdrawn".into())));
    a.push(&a.client(&server));
    b.pull(&b.client(&server));

    let (closed, verdict) = need_verdict(&b, &need);
    assert!(closed, "the pulled withdraw closes the need");
    assert_eq!(verdict.as_deref(), Some("withdrawn"));
    let report = b.mailspace.want_state(&want).unwrap();
    assert_eq!(report.state, WantState::Done, "{report:?}");
    assert_eq!(report.serving[0].verdict.as_deref(), Some("withdrawn"));
    assert_eq!(want_node_state(&b), "done");
}
