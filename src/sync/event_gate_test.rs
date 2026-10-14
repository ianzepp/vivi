//! The event gate: a letter that carries an `X-Vivi-Event` header is a
//! record, not work. It pushes with envelope kind `mail` whatever its item
//! holds, so a client that files by envelope kind can never hold phantom
//! work for it, and a server that shows event letters to every member by
//! header keeps serving them when they stop riding the item's kind.

use std::rc::Rc;

use crate::mailspace::{AbsorbConfig, Mailspace, SendRequest};
use crate::sync::events::state;
use crate::sync::fake_server::{
    Checkout, FakeServer, Item, LETTER_DATE, World, checkout, handle, wire_id,
};
use crate::sync::link;

fn alice() -> Checkout {
    checkout("alice", &["mind"])
}

fn bob() -> Checkout {
    checkout("bob", &["mind", "hand"])
}

/// A third member: she witnesses the work, which is addressed to bob, but
/// she is neither an event letter's sender nor its addressee.
fn carol() -> Checkout {
    checkout("carol", &["mind"])
}

/// Whether raw letter bytes carry an `X-Vivi-Event` header.
fn is_event(bytes: &[u8]) -> bool {
    link::parse_headers(bytes).is_event
}

/// The server records of the event letters: those whose bytes carry the
/// header. Each `kind` is the envelope the fake received.
fn event_records(server: &FakeServer) -> Vec<Item> {
    server
        .items()
        .into_iter()
        .filter(|item| is_event(&item.bytes))
        .collect()
}

/// The content ids of the event letters on the server.
fn event_shas(server: &FakeServer) -> Vec<String> {
    event_records(server)
        .into_iter()
        .map(|item| item.sha)
        .collect()
}

/// Every event letter on the server arrived with envelope kind `mail`.
fn assert_envelopes_are_mail(server: &FakeServer) {
    for item in event_records(server) {
        assert_eq!(item.kind.as_deref(), Some("mail"), "{}", item.message_id);
    }
}

/// No member holds an event letter as work: every row of it is mail, and no
/// backlog node names an event letter.
fn assert_no_phantom_work(members: &[&Checkout], shas: &[String], work: &[&str]) {
    for member in members {
        for row in member.rows() {
            if shas.iter().any(|sha| sha == &row.content_id) {
                assert!(
                    matches!(row.local_role.as_str(), "inbox" | "sent"),
                    "an event letter held as {}",
                    row.local_role
                );
            }
        }
        for (source, _) in nodes(&member.mailspace) {
            assert!(
                work.contains(&source.as_str()),
                "a node for an event letter: {source}"
            );
        }
    }
}

/// The backlog nodes of a mailspace as `(item handle, state)`, sorted. A
/// mailspace that never sent work has no backlog graph, so no nodes.
fn nodes(mailspace: &Mailspace) -> Vec<(String, String)> {
    let Ok(show) = mailspace.graph_show("backlog") else {
        return Vec::new();
    };
    let mut nodes: Vec<(String, String)> = show
        .nodes
        .into_iter()
        .map(|node| (node.source_id, node.state))
        .collect();
    nodes.sort();
    nodes
}

/// Send a task the way `vivi task send` does; returns its handle.
fn send_task(from: &Checkout, to: &str, subject: &str) -> String {
    let result = from
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: format!("{subject} body\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone())
}

/// A mind's close of work for everyone: writes the done event letter.
fn close(a: &Checkout, handle: &str) {
    a.mailspace
        .move_item_outcome(
            "mind",
            handle,
            "done",
            Some("done for everyone"),
            "need done",
            None,
        )
        .unwrap();
}

/// Alice drives a need through its whole lifecycle — mark, done, reopen,
/// bind, done, absorb — writing one event letter per move. Bob holds the
/// work and carol witnesses it; the work has been pushed and pulled once.
struct Gate {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    c: Checkout,
    need: String,
    task: String,
}

fn gate() -> Gate {
    let server = FakeServer::new();
    let (mut a, b, c) = (alice(), bob(), carol());
    // The round closes and absorbs in one breath, so the operator ruling's
    // quiet wait is off here; absorb_quiet_test covers the wait itself.
    a.mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
    let need = a.send_need("mind", &b.address("hand"), "gate the wire");
    let task = send_task(&a, &b.address("hand"), "its unit");
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    c.pull(&c.client(&server));
    a.mailspace
        .set_marks("mind", &need, "need", true, &["ready".into()])
        .unwrap();
    close(&a, &need);
    a.mailspace
        .move_item_outcome("mind", &need, "needs", None, "need reopen", None)
        .unwrap();
    a.mailspace
        .backlog_bind_units(&need, std::slice::from_ref(&task))
        .unwrap();
    close(&a, &need);
    a.mailspace
        .absorb("mind", &need, Some("sealed"), "need")
        .unwrap();
    Gate {
        server,
        a,
        b,
        c,
        need,
        task,
    }
}

/// Pushing the lifecycle letters sends every one of them with envelope kind
/// `mail`, asserted on the envelope the fake received; the work itself
/// keeps its own kind.
#[test]
fn every_lifecycle_letter_pushes_with_envelope_kind_mail() {
    let g = gate();
    g.a.push(&g.a.client(&g.server));

    assert_eq!(
        event_records(&g.server).len(),
        6,
        "one letter per move: marked, done, reopened, bound, done, absorbed"
    );
    assert_envelopes_are_mail(&g.server);
    let mut work: Vec<String> = g
        .server
        .items()
        .into_iter()
        .filter(|item| !is_event(&item.bytes))
        .filter_map(|item| item.kind)
        .collect();
    work.sort();
    assert_eq!(work, ["need".to_string(), "task".to_string()]);
}

/// After a full round no record the fake holds for an event letter carries a
/// work kind, and no member holds one as work: no work row, no node.
#[test]
fn a_full_round_holds_no_event_record_as_work_on_any_member() {
    let g = gate();
    g.a.push(&g.a.client(&g.server));
    let pulled = g.b.pull(&g.b.client(&g.server));
    let witness = g.c.pull(&g.c.client(&g.server));

    // By header, every member is served the events: on the assignee they
    // apply, on the witness they are plain letters.
    assert_eq!(pulled.filed.get("inbox"), Some(&6), "{}", pulled.summary());
    assert_eq!(
        witness.filed.get("inbox"),
        Some(&6),
        "{}",
        witness.summary()
    );

    assert_envelopes_are_mail(&g.server);
    let shas = event_shas(&g.server);
    assert_no_phantom_work(&[&g.a, &g.b, &g.c], &shas, &[&g.need, &g.task]);
}

/// A server that does not serve events by header (`events_by_header =
/// false`) falls back to envelope kinds: the addressed member still gets
/// the event, a non-addressed member does not, and no member holds phantom
/// work for it.
#[test]
fn without_the_header_rule_events_reach_only_their_addressees() {
    let server = FakeServer::new();
    server.set_events_by_header(false);
    let (a, b, c) = (alice(), bob(), carol());
    let need = a.send_need("mind", &b.address("hand"), "gate the wire");
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    c.pull(&c.client(&server));

    close(&a, &need);
    a.push(&a.client(&server));
    assert_envelopes_are_mail(&server);

    let pulled = b.pull(&b.client(&server));
    assert_eq!(
        pulled.filed.get("inbox"),
        Some(&1),
        "{}: the addressee still gets the event",
        pulled.summary()
    );
    assert_eq!(state(&b, &need).as_deref(), Some("done"), "it applies");

    let witness = c.pull(&c.client(&server));
    assert_eq!(
        witness.fetched,
        0,
        "{}: a non-addressed member is not served it",
        witness.summary()
    );

    let shas = event_shas(&server);
    assert_no_phantom_work(&[&a, &b, &c], &shas, &[&need]);
    assert_eq!(
        nodes(&b.mailspace),
        vec![(need.clone(), "done".into())],
        "only the work's own node"
    );
    assert_eq!(nodes(&c.mailspace), Vec::new(), "the witness mints none");
}

/// Pulling a record that carries `X-Vivi-Event` under a work envelope kind —
/// what a mind/sync2-tip client sent — files as mail and mints no node.
#[test]
fn an_event_letter_pulled_with_a_work_envelope_kind_files_as_mail() {
    let w = World::new();
    w.put(1, "item", "done_when: x\n");
    w.bob.pull(&w.bob_client);

    // The event on the wire riding the item's kind, as a mind/sync2-era
    // client pushed it.
    let item_id = format!("{}@x.test", wire_id(1, 0));
    let headers = format!(
        "In-Reply-To: <{item_id}>\r\nX-Vivi-Event: done\r\nX-Vivi-Event-Of: <{item_id}>\r\n"
    );
    w.put_full(
        &wire_id(2, 0),
        "need",
        &w.bob.address("mind"),
        "Re: item",
        "done",
        LETTER_DATE,
        &headers,
    );

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.filed.get("inbox"), Some(&1), "{}", pulled.summary());
    assert_eq!(pulled.filed.get("needs"), None, "no work row from an event");
    assert_eq!(
        state(&w.bob, &handle(1)).as_deref(),
        Some("done"),
        "the event still applies"
    );
    assert_eq!(
        nodes(&w.bob.mailspace),
        vec![(handle(1), "done".into())],
        "no second node from the event letter"
    );
}
