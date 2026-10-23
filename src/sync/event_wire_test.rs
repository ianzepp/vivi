//! The event wire: an event letter is served to every member that holds
//! the item — the server shows it by its `X-Vivi-Event` header, whatever
//! kind its envelope carries — and files as mail whatever kind it carries.

use crate::letter::fixture::need_body;
use crate::mailspace::{Mailspace, RoleUpdate};
use crate::sync::events::{role_of, state};
use crate::sync::fake_server::{Checkout, FakeServer, World, checkout, handle, wire_id};

fn alice() -> Checkout {
    checkout("alice", &["mind"])
}

fn bob() -> Checkout {
    checkout("bob", &["mind", "hand"])
}

/// A third member: she holds the item below only as another mailspace's
/// work, because it is addressed to bob.
fn carol() -> Checkout {
    checkout("carol", &["mind"])
}

fn close(checkout: &Checkout, who: &str, handle: &str) {
    checkout
        .mailspace
        .move_item_outcome(
            who,
            handle,
            "done",
            Some("done for everyone"),
            "need done",
            None,
        )
        .unwrap();
}

fn reopen(checkout: &Checkout, who: &str, handle: &str) {
    checkout
        .mailspace
        .move_item_outcome(who, handle, "needs", None, "need reopen", None)
        .unwrap();
}

/// A role of kind `mind`, which may close or reopen work for everyone.
fn minds(checkout: &mut Checkout, role: &str) {
    checkout
        .mailspace
        .add_role(
            role,
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
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

/// The server item of an event letter: the one whose bytes carry the event
/// header.
fn event_item(server: &FakeServer) -> crate::sync::fake_server::Item {
    server
        .items()
        .into_iter()
        .find(|item| String::from_utf8_lossy(&item.bytes).contains("X-Vivi-Event:"))
        .expect("the close wrote an event letter")
}

#[test]
fn an_event_letter_reaches_every_holder_of_the_item_and_files_as_mail() {
    let server = FakeServer::new();
    let (a, mut b, c) = (alice(), bob(), carol());
    minds(&mut b, "lead");
    let need = a.send_need("mind", &b.address("hand"), "carry the beacon");
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    c.pull(&c.client(&server));

    // Carol holds the need only as bob's work: addressed to his hand, so
    // her row is held, never her own dispatch.
    assert_eq!(role_of(&c, &need), "needs");
    assert_eq!(role_of(&b, &need), "needs");

    close(&a, "mind", &need);
    a.push(&a.client(&server));

    // The event travels as plain mail: the server shows it by its
    // X-Vivi-Event header, so carol — neither its sender nor an addressee
    // of the letter — is served it, and no member can hold phantom work
    // for it by envelope kind.
    assert_eq!(event_item(&server).kind.as_deref(), Some("mail"));

    let pulled = c.pull(&c.client(&server));
    assert_eq!(pulled.filed.get("inbox"), Some(&1), "{}", pulled.summary());
    assert_eq!(role_of(&c, &need), "done", "need list --status done");
    assert_eq!(state(&c, &need).as_deref(), Some("done"), "State: done");

    // The event is mail in carol's store: one row, in the inbox, no work
    // row and no node of its own.
    let sha = event_item(&server).sha;
    let rows: Vec<String> = c
        .rows()
        .into_iter()
        .filter(|row| row.content_id == sha)
        .map(|row| row.local_role)
        .collect();
    assert_eq!(rows, vec!["inbox".to_string()], "held as plain mail");
    assert_eq!(nodes(&c.mailspace), Vec::new(), "no node from a letter");

    // A mind in the holder's mailspace reopens it for everyone.
    b.pull(&b.client(&server));
    reopen(&b, "lead", &need);
    b.push(&b.client(&server));
    for member in [&a, &c] {
        member.pull(&member.client(&server));
        assert_eq!(state(member, &need).as_deref(), Some("reopened"));
    }
    assert_eq!(role_of(&a, &need), "sent", "the author's only copy, open");
    assert_eq!(role_of(&c, &need), "needs", "the held copy, open again");
}

/// [`World::put_full`] with the envelope kind chosen: an event letter put
/// on the wire under its item's kind, as a mind/sync2-era vivi pushed it.
fn put_kind_event(w: &World, id: u64, item: u64, kind: &str) {
    let item_id = format!("{}@x.test", wire_id(item, 0));
    let headers = format!(
        "In-Reply-To: <{item_id}>\r\nX-Vivi-Event: {kind}\r\nX-Vivi-Event-Of: <{item_id}>\r\n"
    );
    w.put_full(
        &wire_id(id, 0),
        "need",
        &w.bob.address("mind"),
        "Re: item",
        kind,
        crate::sync::fake_server::LETTER_DATE,
        &headers,
    );
}

#[test]
fn an_event_letter_files_as_mail_and_mints_no_node_whatever_kind_it_carries() {
    let w = World::new();
    w.put(1, "item", &need_body(&["x"], ""));
    w.bob.pull(&w.bob_client);
    assert_eq!(nodes(&w.bob.mailspace), vec![(handle(1), "open".into())]);

    // A done event riding the need's kind, as a mind/sync2-era client sent
    // it: the filer must see no kind for it, or it would file work rows and
    // mint a backlog node of its own.
    put_kind_event(&w, 2, 1, "done");

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.filed.get("inbox"), Some(&1), "{}", pulled.summary());
    assert_eq!(pulled.filed.get("needs"), None, "no work row from an event");
    assert_eq!(
        role_of(&w.bob, &handle(1)),
        "done",
        "the event still applies"
    );
    assert_eq!(
        nodes(&w.bob.mailspace),
        vec![(handle(1), "done".into())],
        "no second node from the event letter"
    );
}
