//! The absorb letter's write side: an absorb by the item's author (or a
//! mind or operator) writes an `absorbed` reply letter that travels through
//! sync and seals every holder's copy in the state it names; a role that
//! may not close the item seals only its own copy and writes no letter.

use crate::sync::fake_server::Checkout;

const EVENT: &str = "X-Vivi-Event";
const STATE: &str = "X-Vivi-Event-State";

/// The value of a letter's header, when it has one.
fn header_of(bytes: &[u8], name: &str) -> Option<String> {
    let parsed = mail_parser::MessageParser::default().parse(bytes)?;
    parsed
        .headers()
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case(name))
        .and_then(|header| match header.value() {
            mail_parser::HeaderValue::Text(text) => Some(text.trim().to_string()),
            _ => None,
        })
}

/// The bytes of every letter held on `checkout` whose `X-Vivi-Event` is
/// `event`.
fn event_letters(checkout: &Checkout, event: &str) -> Vec<Vec<u8>> {
    checkout
        .rows()
        .into_iter()
        .map(|row| checkout.bytes_of(&row.message_id))
        .filter(|bytes| header_of(bytes, EVENT).as_deref() == Some(event))
        .collect()
}

/// The folder and seal of the item's row on `checkout`.
fn item_row(checkout: &Checkout, handle: &str) -> (String, bool) {
    let row = checkout.row(&checkout.id_of(handle)).unwrap();
    (row.local_role, row.absorbed_at.is_some())
}

#[test]
fn the_authors_absorb_letter_seals_the_assignees_copy_done_and_blocks_reopen() {
    let p = super::pair();
    super::close(&p.a, "mind", &p.task, None);
    assert_eq!(super::role_of(&p.a, &p.task), "done");

    let absorbed = p.a.mailspace.absorb("mind", &p.task, None, "task").unwrap();

    assert_eq!(absorbed, p.task, "absorb still returns the item's handle");
    let letters = event_letters(&p.a, "absorbed");
    assert_eq!(letters.len(), 1, "one absorbed letter beside the close");
    let item_id = super::rfc_id(&p.a.bytes_of(&p.task));
    assert_eq!(header_of(&letters[0], STATE).as_deref(), Some("done"));
    assert!(header_of(&letters[0], "X-Vivi-Event-At").is_some());
    assert!(
        header_of(&letters[0], "In-Reply-To").is_some_and(|v| v.contains(&item_id)),
        "it replies to the item"
    );

    let pulled = p.sync_a_to_b();

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(item_row(&p.b, &p.task), ("done".into(), true));
    assert_eq!(super::node_state(&p.b.mailspace, &p.task), "done");
    let refused =
        p.b.mailspace
            .move_task("hand", &p.task, "tasks", None, None, &[], &[])
            .unwrap_err()
            .to_string();
    assert!(refused.contains("absorbed"), "{refused}");
}

#[test]
fn an_absorb_of_an_open_item_is_refused_and_a_closed_absorb_seals_every_member_done() {
    let p = super::pair();

    let refused =
        p.a.mailspace
            .absorb("mind", &p.task, None, "task")
            .unwrap_err()
            .to_string();
    assert!(refused.contains("close it before absorbing"), "{refused}");
    assert!(event_letters(&p.a, "absorbed").is_empty(), "no letter");

    super::close(&p.a, "mind", &p.task, None);

    p.a.mailspace.absorb("mind", &p.task, None, "task").unwrap();

    let letters = event_letters(&p.a, "absorbed");
    assert_eq!(letters.len(), 1);
    assert_eq!(header_of(&letters[0], STATE).as_deref(), Some("done"));

    let pulled = p.sync_a_to_b();

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(item_row(&p.a, &p.task), ("done".into(), true));
    assert_eq!(item_row(&p.b, &p.task), ("done".into(), true));
    assert_eq!(super::node_state(&p.b.mailspace, &p.task), "done");
}

#[test]
fn a_hands_absorb_writes_no_letter_and_leaves_the_authors_copy_unsealed() {
    let p = super::pair();

    p.b.mailspace.absorb("hand", &p.task, None, "task").unwrap();

    assert!(event_letters(&p.b, "absorbed").is_empty(), "no letter");
    assert_eq!(item_row(&p.b, &p.task), ("tasks".into(), true));

    let pulled = p.sync_b_to_a();

    assert!(
        pulled.filed.is_empty(),
        "no letter travelled: {}",
        pulled.summary()
    );
    assert_eq!(item_row(&p.a, &p.task), ("sent".into(), false));
    assert!(event_letters(&p.a, "absorbed").is_empty());
}
