//! Bind convergence: the controlling bind of a unit is the newest counted
//! `bound` letter naming it by `(X-Vivi-Event-At, Message-ID)`, across every
//! need held — a function of letters only — so every member that pulls the
//! same letters lands a re-targeted unit on the same need, whatever the
//! arrival or settle order. A receiver never stamps its own clock as a
//! floor.

use std::cmp::Ordering;
use std::rc::Rc;
use std::thread::sleep;
use std::time::Duration;

use crate::letter::fixture::{body_for, task_body};
use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::BACKLOG_GRAPH_CODE;
use crate::sync::fake_server::{Checkout, FakeServer, checkout, wire_id};
use crate::sync::transport::{Envelope, Transport};

#[path = "property_bind_test.rs"]
mod property_bind_test;

fn alice() -> Checkout {
    checkout("alice", &["mind"])
}

fn bob() -> Checkout {
    checkout("bob", &["mind", "hand"])
}

fn carol() -> Checkout {
    checkout("carol", &["mind"])
}

/// The fresh member: she holds every address but never pulls until a test
/// asks her to, so her first pull carries everything at once.
fn dave() -> Checkout {
    checkout("dave", &["mind"])
}

fn send_task(from: &Checkout, to: &str, subject: &str) -> String {
    let result = from
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: task_body("it works\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone())
}

fn close(checkout: &Checkout, who: &str, handle: &str, note: Option<&str>) {
    checkout
        .mailspace
        .move_task(who, handle, "done", note, None, &[], &[])
        .unwrap();
}

fn node_state(mailspace: &Mailspace, handle: &str) -> String {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == handle)
        .unwrap_or_else(|| panic!("no node for {handle}"))
        .state
}

pub(super) fn role_of(checkout: &Checkout, handle: &str) -> String {
    checkout.row(&checkout.id_of(handle)).unwrap().local_role
}

fn rfc_id(bytes: &[u8]) -> String {
    mail_parser::MessageParser::default()
        .parse(bytes)
        .unwrap()
        .message_id()
        .unwrap()
        .to_string()
}

/// The writer, the holder, the third member, and the fresh member, with a
/// server every letter travels through.
struct World {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    c: Checkout,
    d: Checkout,
}

fn world() -> World {
    let server = FakeServer::new();
    World {
        server,
        a: alice(),
        b: bob(),
        c: carol(),
        d: dave(),
    }
}

impl World {
    /// Every work item is addressed to the holder's hand, the third
    /// member's mind, and the fresh member's mind, so each of them holds
    /// the items as its own work with backlog nodes.
    fn to(&self) -> Vec<String> {
        vec![
            self.b.address("hand"),
            self.c.address("mind"),
            self.d.address("mind"),
        ]
    }
}

fn send_work(from: &Checkout, role: &str, kind: &str, to: &[String], subject: &str) -> String {
    let result = from
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: to.to_vec(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: body_for(kind, "it works\n"),
            role: role.into(),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap();
    result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone())
}

/// The unit source ids bound to `need`'s backlog node here, sorted.
fn bound_units(mailspace: &Mailspace, need: &str) -> Vec<String> {
    let storage = mailspace.storage().unwrap();
    let graph = storage
        .work_graph_by_code(BACKLOG_GRAPH_CODE)
        .unwrap()
        .expect("the backlog graph exists");
    let mut sources: Vec<String> = storage
        .work_graph_nodes_by_subgraph(&graph.handle, need)
        .unwrap()
        .into_iter()
        .map(|node| node.source_id)
        .collect();
    sources.sort();
    sources
}

/// Every value of a letter header, when the letter has any.
fn headers_of(bytes: &[u8], name: &str) -> Vec<String> {
    let Some(parsed) = mail_parser::MessageParser::default().parse(bytes) else {
        return Vec::new();
    };
    parsed
        .headers()
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case(name))
        .filter_map(|header| match header.value() {
            mail_parser::HeaderValue::Text(text) => Some(text.trim().to_string()),
            _ => None,
        })
        .collect()
}

/// The one `bound` letter held on `checkout` that replies to `need`.
fn bound_letter(checkout: &Checkout, need: &str) -> Vec<u8> {
    let of = rfc_id(&checkout.bytes_of(need)).to_ascii_lowercase();
    let letters: Vec<Vec<u8>> = checkout
        .rows()
        .into_iter()
        .map(|row| checkout.bytes_of(&row.message_id))
        .filter(|bytes| {
            headers_of(bytes, "X-Vivi-Event") == ["bound".to_string()]
                && headers_of(bytes, "In-Reply-To")
                    .iter()
                    .any(|value| value.to_ascii_lowercase().contains(&of))
        })
        .collect();
    assert_eq!(letters.len(), 1, "one bound letter on {need}");
    letters.into_iter().next().unwrap()
}

/// The content id a handle's letter has on `checkout`.
fn content_of(checkout: &Checkout, handle: &str) -> String {
    checkout.row(&checkout.id_of(handle)).unwrap().content_id
}

/// One re-targeting pair under `tag`: the first home, the re-target, and a
/// unit, all sent by `a`, held by `b` and `c`, waiting for `d`. Returns the
/// two need handles, the unit, and whether the first home's content id
/// sorts before the re-target's — the order `apply_arrived_events` settles
/// the two needs in when both letters arrive in one pull.
fn pair(w: &World, tag: &str) -> (String, String, String, Ordering) {
    let to = w.to();
    let one = send_work(&w.a, "needs", "need", &to, &format!("first home {tag}"));
    let two = send_work(&w.a, "needs", "need", &to, &format!("second home {tag}"));
    let unit = send_work(&w.a, "tasks", "task", &to, &format!("unit {tag}"));
    w.a.push(&w.a.client(&w.server));
    w.b.pull(&w.b.client(&w.server));
    w.c.pull(&w.c.client(&w.server));
    let order = content_of(&w.c, &one).cmp(&content_of(&w.c, &two));
    (one, two, unit, order)
}

/// `a` binds `unit` to `one`, then — strictly later on the clock —
/// re-targets it to `two`: two `bound` letters, the second newer.
fn bind_and_retarget(w: &World, one: &str, two: &str, unit: &str) {
    w.a.mailspace
        .backlog_bind_units(one, &[unit.to_string()])
        .unwrap();
    sleep(Duration::from_millis(2));
    w.a.mailspace
        .backlog_bind_units(two, &[unit.to_string()])
        .unwrap();
}

#[test]
fn two_pulls_move_the_unit_to_the_retargeted_need() {
    let w = world();
    let (one, two, unit, _) = pair(&w, "two pulls");
    bind_and_retarget(&w, &one, &two, &unit);
    // Both letters are written before either arrives: the older one is put
    // on the wire by hand, the newer one pushed only after the third
    // member has pulled it, so the two letters land in two pulls.
    let older = bound_letter(&w.a, &one);
    let to = w.to().join(", ");
    w.a.client(&w.server)
        .append(
            &wire_id(90, 1),
            &Envelope {
                raw_eml: &older,
                to: &to,
                kind: "need",
            },
        )
        .unwrap();
    w.c.pull(&w.c.client(&w.server));
    assert_eq!(
        bound_units(&w.c.mailspace, &one),
        vec![unit.clone()],
        "the first letter binds"
    );

    w.a.push(&w.a.client(&w.server));
    w.c.pull(&w.c.client(&w.server));

    assert_eq!(
        bound_units(&w.c.mailspace, &two),
        vec![unit.clone()],
        "the newer letter re-targets across pulls"
    );
    assert!(
        bound_units(&w.c.mailspace, &one).is_empty(),
        "the older letter moved nothing back"
    );
}

#[test]
fn one_pull_with_both_letters_lands_the_retarget_in_either_settle_order() {
    let w = world();
    let mut seen: Vec<Ordering> = Vec::new();
    for n in 0..40 {
        if seen.len() == 2 {
            break;
        }
        let (one, two, unit, order) = pair(&w, &format!("one pull {n}"));
        if seen.contains(&order) {
            continue;
        }
        bind_and_retarget(&w, &one, &two, &unit);
        w.a.push(&w.a.client(&w.server));
        w.c.pull(&w.c.client(&w.server));
        assert_eq!(
            bound_units(&w.c.mailspace, &two),
            vec![unit.clone()],
            "settle order {order:?}"
        );
        assert!(
            bound_units(&w.c.mailspace, &one).is_empty(),
            "settle order {order:?}"
        );
        seen.push(order);
    }
    assert_eq!(seen.len(), 2, "both settle orders were exercised");
}

#[test]
fn a_fresh_member_pulling_everything_lands_the_retarget() {
    let w = world();
    let (one, two, unit, _) = pair(&w, "fresh");
    bind_and_retarget(&w, &one, &two, &unit);
    w.a.push(&w.a.client(&w.server));
    w.d.pull(&w.d.client(&w.server));
    assert_eq!(
        bound_units(&w.d.mailspace, &two),
        vec![unit.clone()],
        "one pull from empty lands the newer bind"
    );
    assert!(bound_units(&w.d.mailspace, &one).is_empty());
}

#[test]
fn the_reverse_arrival_order_lands_the_retarget_too() {
    let w = world();
    let (one, two, unit, _) = pair(&w, "reverse");
    bind_and_retarget(&w, &one, &two, &unit);
    // The newer letter goes on the wire first, by hand; the older one is
    // pushed only after the third member has pulled it.
    let newer = bound_letter(&w.a, &two);
    let to = w.to().join(", ");
    w.a.client(&w.server)
        .append(
            &wire_id(91, 1),
            &Envelope {
                raw_eml: &newer,
                to: &to,
                kind: "need",
            },
        )
        .unwrap();
    w.c.pull(&w.c.client(&w.server));
    assert_eq!(
        bound_units(&w.c.mailspace, &two),
        vec![unit.clone()],
        "the newer letter binds first"
    );

    w.a.push(&w.a.client(&w.server));
    w.c.pull(&w.c.client(&w.server));

    assert_eq!(
        bound_units(&w.c.mailspace, &two),
        vec![unit.clone()],
        "the older letter arriving later moves nothing"
    );
    assert!(bound_units(&w.c.mailspace, &one).is_empty());
}
