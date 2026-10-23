//! Need lowering as a letter: a `need bind` writes one `bound` property
//! letter on the need, and every member that holds the units binds them to
//! its own backlog node for the need, so the join completes the need
//! wherever its units land. A letter older than a local bind moves nothing.

use std::rc::Rc;

use super::{alice, bob, close, node_state, rfc_id, role_of, send_task};
use crate::letter::fixture::body_for;
use crate::mailspace::{Mailspace, SendRequest};
use crate::storage::BACKLOG_GRAPH_CODE;
use crate::sync::fake_server::{Checkout, FakeServer, letter, wire_id};
use crate::sync::transport::{Envelope, Transport};

/// The writer (alice) and the holder (bob) of one lowered need.
struct Pair {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
}

fn pair() -> Pair {
    let server = FakeServer::new();
    Pair {
        server,
        a: alice(),
        b: bob(),
    }
}

fn send_work(from: &Checkout, role: &str, kind: &str, to: &str, subject: &str) -> String {
    let result = from
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: body_for(kind, "it works\n"),
            role: role.into(),
            kind: Some(kind.into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone())
}

fn send_need(from: &Checkout, to: &str, subject: &str) -> String {
    send_work(from, "needs", "need", to, subject)
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

fn sorted(mut handles: Vec<String>) -> Vec<String> {
    handles.sort();
    handles
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

/// The `bound` letters held on `checkout` that reply to `need`.
fn bound_letters(checkout: &Checkout, need: &str) -> Vec<Vec<u8>> {
    let of = rfc_id(&checkout.bytes_of(need)).to_ascii_lowercase();
    checkout
        .rows()
        .into_iter()
        .map(|row| checkout.bytes_of(&row.message_id))
        .filter(|bytes| {
            headers_of(bytes, "X-Vivi-Event") == ["bound".to_string()]
                && headers_of(bytes, "In-Reply-To")
                    .iter()
                    .any(|value| value.to_ascii_lowercase().contains(&of))
        })
        .collect()
}

/// A need on `a` with two units, all filed on `b` (its `hand`), bound on `a`.
fn lowered_pair() -> (Pair, String, String, String) {
    let p = pair();
    let to = p.b.address("hand");
    let need = send_need(&p.a, &to, "lower me");
    let first = send_task(&p.a, &to, "unit 1");
    let second = send_task(&p.a, &to, "unit 2");
    p.a.push(&p.a.client(&p.server));
    p.b.pull(&p.b.client(&p.server));
    p.a.mailspace
        .backlog_bind_units(&need, &[first.clone(), second.clone()])
        .unwrap();
    p.a.push(&p.a.client(&p.server));
    p.b.pull(&p.b.client(&p.server));
    (p, need, first, second)
}

#[test]
fn a_bind_writes_one_bound_letter_that_binds_the_units_on_the_holder() {
    let (p, need, first, second) = lowered_pair();

    let letters = bound_letters(&p.a, &need);
    assert_eq!(letters.len(), 1, "one bound letter on the need");
    let units: Vec<String> = headers_of(&letters[0], "X-Vivi-Event-Unit");
    assert_eq!(units.len(), 2, "one unit header per unit: {letters:?}");
    assert_eq!(
        bound_units(&p.a.mailspace, &need),
        sorted(vec![first.clone(), second.clone()]),
        "the writer binds its own nodes"
    );
    assert_eq!(
        bound_units(&p.b.mailspace, &need),
        sorted(vec![first.clone(), second.clone()]),
        "the holder binds its nodes from the letter"
    );
    assert_eq!(node_state(&p.b.mailspace, &first), "open");
    assert_eq!(node_state(&p.b.mailspace, &need), "open");
}

#[test]
fn the_join_completes_the_need_on_the_holder_when_every_unit_lands() {
    let (p, need, first, second) = lowered_pair();

    close(&p.a, "mind", &first, None);
    p.a.push(&p.a.client(&p.server));
    p.b.pull(&p.b.client(&p.server));
    assert_eq!(node_state(&p.b.mailspace, &first), "done");
    assert_eq!(node_state(&p.b.mailspace, &need), "open");

    close(&p.a, "mind", &second, None);
    p.a.push(&p.a.client(&p.server));
    p.b.pull(&p.b.client(&p.server));

    assert_eq!(node_state(&p.b.mailspace, &second), "done");
    assert_eq!(node_state(&p.b.mailspace, &need), "done", "the join ran");
    assert_eq!(role_of(&p.b, &need), "done", "the mailbox settled");
    assert_eq!(node_state(&p.a.mailspace, &need), "done");
}

#[test]
fn an_older_bound_letter_does_not_rebind_a_unit_a_newer_local_bind_moved() {
    let p = pair();
    let to = p.b.address("hand");
    let old_home = send_need(&p.a, &to, "the old home");
    let new_home = send_need(&p.a, &to, "the new home");
    let unit = send_task(&p.a, &to, "unit");
    p.a.push(&p.a.client(&p.server));
    p.b.pull(&p.b.client(&p.server));

    // The holder binds the unit to the other need; an older letter from the
    // writer names the same unit for the first need.
    p.b.mailspace
        .backlog_bind_units(&new_home, &[unit.clone()])
        .unwrap();
    let old = rfc_id(&p.b.bytes_of(&old_home));
    let unit_id = rfc_id(&p.b.bytes_of(&unit));
    let headers = format!(
        "In-Reply-To: <{old}>\r\nX-Vivi-Event: bound\r\nX-Vivi-Event-Of: <{old}>\r\n\
         X-Vivi-Event-Unit: <{unit_id}>\r\n"
    );
    let id = wire_id(90, 0);
    let bytes = letter(
        &id,
        &p.a.address("mind"),
        &to,
        "Re: the old home",
        "bound",
        "Mon, 01 Jan 2001 00:00:00 +0000",
        &headers,
    );
    p.a.client(&p.server)
        .append(
            &id,
            &Envelope {
                raw_eml: &bytes,
                to: &to,
                kind: "mail",
            },
        )
        .unwrap();

    let pulled = p.b.pull(&p.b.client(&p.server));

    assert_eq!(pulled.events_applied, 0, "{}", pulled.summary());
    assert!(
        bound_units(&p.b.mailspace, &old_home).is_empty(),
        "the older letter moved nothing"
    );
    assert_eq!(
        bound_units(&p.b.mailspace, &new_home),
        vec![unit.clone()],
        "the newer local bind holds"
    );
}
