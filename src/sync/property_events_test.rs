//! Property events across mailspaces: an absorbed event letter seals its
//! item here in the state the writer sealed it in, and every state event
//! beside or after the seal is only counted.

use super::node_state;
use crate::letter::fixture::need_body;
use crate::sync::fake_server::{LETTER_DATE, World, handle, letter, wire_id};
use crate::sync::transport::{Envelope, Transport};

/// The reply headers every event letter carries, plus `extra` payload ones.
fn event_headers(item: u64, kind: &str, extra: &str) -> String {
    let item = format!("{}@x.test", wire_id(item, 0));
    format!("In-Reply-To: <{item}>\r\nX-Vivi-Event: {kind}\r\nX-Vivi-Event-Of: <{item}>\r\n{extra}")
}

/// An event letter from alice (a prefix domain) to bob's `mind`.
fn put_event(w: &World, id: u64, item: u64, kind: &str, date: &str, extra: &str) {
    w.put_full(
        &wire_id(id, 0),
        "mail",
        &w.bob.address("mind"),
        "Re: item",
        kind,
        date,
        &event_headers(item, kind, extra),
    );
}

fn put_absorbed(w: &World, id: u64, item: u64, state: &str) {
    put_event(
        w,
        id,
        item,
        "absorbed",
        LETTER_DATE,
        &format!("X-Vivi-Event-State: {state}\r\n"),
    );
}

fn put_state(w: &World, id: u64, item: u64, kind: &str, date: &str) {
    put_event(w, id, item, kind, date, "");
}

/// The folder of the item's row on bob, and whether the row is sealed.
fn item_row(w: &World, item: u64) -> (String, bool) {
    let row = w.bob.row(&w.bob.id_of(&handle(item))).unwrap();
    (row.local_role, row.absorbed_at.is_some())
}

#[test]
fn an_absorbed_event_with_state_done_seals_the_item_here_in_done() {
    let w = World::new();
    w.put(1, "item", &need_body(&["x"], ""));
    w.bob.pull(&w.bob_client);
    put_absorbed(&w, 2, 1, "done");

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(pulled.events_ignored, 0);
    assert_eq!(item_row(&w, 1), ("done".into(), true));
    assert_eq!(node_state(&w.bob.mailspace, &handle(1)), "done");
}

#[test]
fn an_absorbed_event_naming_open_leaves_the_item_open_and_sealed() {
    let w = World::new();
    w.put(1, "item", &need_body(&["x"], ""));
    w.bob.pull(&w.bob_client);
    put_absorbed(&w, 2, 1, "open");

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(item_row(&w, 1), ("needs".into(), true));
    assert_eq!(node_state(&w.bob.mailspace, &handle(1)), "open");
}

#[test]
fn after_the_seal_a_later_reopen_is_ignored_and_counted() {
    let w = World::new();
    w.put(1, "item", &need_body(&["x"], ""));
    w.bob.pull(&w.bob_client);
    put_absorbed(&w, 2, 1, "done");
    w.bob.pull(&w.bob_client);

    // A reopen newer than the seal: the absorb letter already named the
    // state, so the event is only counted.
    put_state(&w, 3, 1, "reopened", "Sun, 11 Oct 2026 13:00:00 +0000");
    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_applied, 0);
    assert_eq!(pulled.events_ignored, 1, "{}", pulled.summary());
    assert_eq!(item_row(&w, 1), ("done".into(), true));
    assert_eq!(node_state(&w.bob.mailspace, &handle(1)), "done");
}

#[test]
fn an_older_done_event_arriving_after_the_absorb_changes_nothing() {
    let w = World::new();
    w.put(1, "item", &need_body(&["x"], ""));
    w.bob.pull(&w.bob_client);
    put_absorbed(&w, 2, 1, "open");
    w.bob.pull(&w.bob_client);
    assert_eq!(item_row(&w, 1), ("needs".into(), true));

    put_state(&w, 3, 1, "done", "Sun, 11 Oct 2026 11:00:00 +0000");
    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_applied, 0);
    assert_eq!(pulled.events_ignored, 1, "{}", pulled.summary());
    assert_eq!(item_row(&w, 1), ("needs".into(), true), "the seal holds");
    assert_eq!(node_state(&w.bob.mailspace, &handle(1)), "open");
}

#[test]
fn an_absorbed_event_from_a_bare_or_legacy_domain_is_ignored() {
    let w = World::new();
    w.put(1, "item", &need_body(&["x"], ""));
    w.bob.pull(&w.bob_client);
    let to = w.bob.address("mind");
    let headers = event_headers(1, "absorbed", "X-Vivi-Event-State: done\r\n");
    let legacy = format!("mind@{}", w.alice.mailspace.legacy_domain());
    let client = w.bob.client(&w.server);
    for (n, from) in ["mind@local", legacy.as_str()].into_iter().enumerate() {
        let id = wire_id(20 + n as u64, 0);
        let bytes = letter(
            &id,
            from,
            &to,
            "Re: item",
            "absorbed",
            LETTER_DATE,
            &headers,
        );
        client
            .append(
                &id,
                &Envelope {
                    raw_eml: &bytes,
                    to: &to,
                    kind: "mail",
                },
            )
            .unwrap();
    }

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_applied, 0);
    assert_eq!(pulled.events_ignored, 2, "{}", pulled.summary());
    assert_eq!(item_row(&w, 1), ("needs".into(), false));
}

#[test]
fn an_absorbed_event_that_arrives_before_its_item_is_applied_when_the_item_arrives() {
    let w = World::new();
    put_absorbed(&w, 2, 1, "done");
    w.put(1, "item", &need_body(&["x"], ""));

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(item_row(&w, 1), ("done".into(), true));
    assert_eq!(node_state(&w.bob.mailspace, &handle(1)), "done");
}
