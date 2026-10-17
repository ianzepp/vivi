//! Foreign rows: an item held only as a witness of another mailspace's work
//! moves with its event letters, a seal keeps the event out, and audit repair
//! re-applies a stale foreign row.

use super::events::{BODY, put_event, role_of, state};
use crate::mailspace::is_foreign_account;
use crate::sync::fake_server::{Checkout, LETTER_DATE, World, checkout, handle, wire_id};

// A third mailspace: the work lands there, and bob only witnesses it.
fn carol() -> Checkout {
    checkout("carol", &["mind"])
}

/// A need from alice addressed only to carol's `mind`, pulled by bob. Work
/// is visible to every member, so bob holds it under carol's raw address: a
/// foreign row, with no backlog node of its own.
struct Foreign {
    #[allow(dead_code)]
    carol: Checkout,
    handle: String,
    account: String,
}

fn foreign_need(w: &World) -> Foreign {
    // bob's mailspace holds its own work as well, so its backlog graph
    // exists and the tests can watch the foreign item stay out of it.
    w.bob
        .send_need("hand", &w.alice.address("mind"), "bob's own");
    let carol = carol();
    w.put_full(
        &wire_id(1, 0),
        "need",
        &carol.address("mind"),
        "for carol",
        BODY,
        LETTER_DATE,
        "",
    );
    w.bob.pull(&w.bob_client);
    let handle = handle(1);
    let row = w.bob.row(&w.bob.id_of(&handle)).unwrap();
    Foreign {
        carol,
        account: row.account.clone(),
        handle,
    }
}

/// Whether `checkout` holds no backlog node for `handle`.
fn no_node(checkout: &Checkout, handle: &str) -> bool {
    !checkout
        .mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .iter()
        .any(|node| node.source_id == handle)
}

// The state of an item is what its newest event says, on every member that
// holds it — one that only witnesses the work included. The event letter
// itself is mail, so the fake server delivers it only to its addressees:
// the put helpers address it to bob, the witnessing member here.
#[test]
fn a_need_held_only_foreign_moves_with_its_events() {
    let w = World::new();
    let fx = foreign_need(&w);
    assert!(is_foreign_account(&fx.account), "{}", fx.account);
    assert_eq!(role_of(&w.bob, &fx.handle), "needs");
    assert!(no_node(&w.bob, &fx.handle), "a foreign row mints no node");

    put_event(&w, 2, 1, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");
    let pulled = w.bob.pull(&w.bob_client);
    assert_eq!(pulled.events_applied, 1);
    assert_eq!(pulled.events_ignored, 0);
    assert_eq!(
        role_of(&w.bob, &fx.handle),
        "done",
        "the foreign row closed"
    );
    assert_eq!(state(&w.bob, &fx.handle).as_deref(), Some("done"));
    assert!(no_node(&w.bob, &fx.handle), "and still no node of it");

    put_event(
        &w,
        3,
        1,
        "reopened",
        &[2],
        "Sun, 11 Oct 2026 12:01:00 +0000",
    );
    w.bob.pull(&w.bob_client);
    assert_eq!(
        role_of(&w.bob, &fx.handle),
        "needs",
        "back to the folder it closed from"
    );
    assert_eq!(state(&w.bob, &fx.handle).as_deref(), Some("reopened"));
}

#[test]
fn an_absorbed_foreign_row_does_not_move_and_the_event_is_ignored() {
    let w = World::new();
    let fx = foreign_need(&w);
    // The lifecycle refuses to seal another mailspace's work, so the test
    // seals the row at the storage level, as a restored archive row reads.
    let storage = w.bob.mailspace.storage().unwrap();
    storage
        .absorb_message(&fx.account, &w.bob.id_of(&fx.handle), "mind")
        .unwrap();
    drop(storage);

    put_event(&w, 2, 1, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");
    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_ignored, 1);
    assert_eq!(pulled.events_applied, 0);
    assert_eq!(role_of(&w.bob, &fx.handle), "needs", "the seal holds");
}

#[test]
fn an_audit_repair_moves_a_stale_foreign_row() {
    let w = World::new();
    let fx = foreign_need(&w);
    put_event(&w, 2, 1, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");
    w.bob.pull(&w.bob_client);
    assert_eq!(role_of(&w.bob, &fx.handle), "done");

    // Roll the foreign row back by hand, as a checkout copied from before
    // the event is.
    let row = w.bob.row(&w.bob.id_of(&fx.handle)).unwrap();
    let storage = w.bob.mailspace.storage().unwrap();
    storage
        .move_message_to_role(&row.account, &row.message_id, "needs")
        .unwrap();
    drop(storage);

    let audit = w.bob.mailspace.backlog_audit(false).unwrap();
    assert!(
        audit.findings.iter().any(|f| f.class == "event_state"),
        "{:?}",
        audit.findings
    );
    let repaired = w.bob.mailspace.backlog_audit(true).unwrap();
    assert!(repaired.findings.iter().all(|f| f.repaired));
    assert_eq!(role_of(&w.bob, &fx.handle), "done", "the repair applied it");
    let clean = w.bob.mailspace.backlog_audit(true).unwrap();
    assert!(clean.findings.is_empty(), "{:?}", clean.findings);
}
