//! Marks as letters across mailspaces: a mark or unmark by any member that
//! holds the work writes a reply letter beside the item, travels through the
//! fake project server, and the newest event per slug controls on every
//! member — while a letter older than a row's newest local mark event for
//! the slug changes nothing.

use std::rc::Rc;

use super::{alice, bob};
use crate::mailspace::{RoleUpdate, is_foreign_account};
use crate::sync::fake_server::{
    Checkout, FakeServer, LETTER_DATE, World, checkout, handle, wire_id,
};

const TRAVEL: &str = "travel";

/// One pair of mailspaces with a need from alice's `mind` to bob's `mind`,
/// filed on both sides.
struct Pair {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    need: String,
}

fn pair() -> Pair {
    let server = FakeServer::new();
    let (mut a, mut b) = (alice(), bob());
    minds(&mut a);
    minds(&mut b);
    let need = a.send_need("mind", &b.address("mind"), "travel policy");
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    Pair { server, a, b, need }
}

/// Bind each member's `mind` seat to kind `mind`, the way
/// `event_wire_test::minds` does: only the item's author or a role of that
/// kind may close work for everyone, so a mark by any other seat stays
/// private and never travels.
fn minds(checkout: &mut Checkout) {
    checkout
        .mailspace
        .set_role(
            "mind",
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
}

impl Pair {
    fn sync_a_to_b(&self) -> crate::sync::pull::PullReport {
        self.a.push(&self.a.client(&self.server));
        self.b.pull(&self.b.client(&self.server))
    }

    fn sync_b_to_a(&self) -> crate::sync::pull::PullReport {
        self.b.push(&self.b.client(&self.server));
        self.a.pull(&self.a.client(&self.server))
    }
}

/// Mark or unmark the slug on `checkout`, the way `vivi need mark` does.
fn mark(checkout: &Checkout, who: &str, handle: &str, add: bool) {
    checkout
        .mailspace
        .set_marks(who, handle, "need", add, &[TRAVEL.to_string()])
        .unwrap();
}

/// The folded marks of the item's own row, the way `need|want show` prints
/// them: `Marks:` from the folded events.
fn marks_of(checkout: &Checkout, item: &str) -> Vec<String> {
    let id = checkout.id_of(item);
    checkout
        .mailspace
        .thread(item, false, 50, 50)
        .unwrap()
        .into_iter()
        .find(|message| message.message_id == id)
        .unwrap_or_else(|| panic!("the item is not in its own thread"))
        .marks
}

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

#[test]
fn a_mark_travels_as_a_letter_and_shows_on_the_other_member() {
    let p = pair();
    mark(&p.a, "mind", &p.need, true);
    assert_eq!(marks_of(&p.a, &p.need), vec![TRAVEL.to_string()]);

    // The mark is a reply letter beside the item, carrying its slug.
    let letter =
        p.a.rows()
            .into_iter()
            .map(|row| p.a.bytes_of(&row.message_id))
            .find(|bytes| header_of(bytes, "X-Vivi-Event").as_deref() == Some("marked"))
            .expect("the mark wrote a letter");
    assert_eq!(
        header_of(&letter, "X-Vivi-Event-Slug").as_deref(),
        Some(TRAVEL)
    );

    let pulled = p.sync_a_to_b();

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(marks_of(&p.b, &p.need), vec![TRAVEL.to_string()]);
    // Idempotent: pulling the same letters again changes nothing, and
    // `need show` keeps printing the folded state.
    p.sync_a_to_b();
    assert_eq!(marks_of(&p.b, &p.need), vec![TRAVEL.to_string()]);
}

#[test]
fn an_unmark_on_the_other_member_travels_back() {
    let p = pair();
    mark(&p.a, "mind", &p.need, true);
    p.sync_a_to_b();
    assert_eq!(marks_of(&p.b, &p.need), vec![TRAVEL.to_string()]);

    mark(&p.b, "mind", &p.need, false);
    assert_eq!(marks_of(&p.b, &p.need), Vec::<String>::new());
    let pulled = p.sync_b_to_a();

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(
        marks_of(&p.a, &p.need),
        Vec::<String>::new(),
        "A shows none"
    );
}

#[test]
fn two_unseen_mark_events_end_with_the_newest_one_on_both() {
    let p = pair();
    // B marks first, A marks without having seen B's letter, and B unmarks
    // without having seen A's: the unmark is the newest event for the slug.
    mark(&p.b, "mind", &p.need, true);
    mark(&p.a, "mind", &p.need, true);
    mark(&p.b, "mind", &p.need, false);

    let pulled_a = p.sync_b_to_a();
    let pulled_b = p.sync_a_to_b();

    assert_eq!(pulled_a.events_applied, 1, "{}", pulled_a.summary());
    assert_eq!(marks_of(&p.a, &p.need), Vec::<String>::new());
    assert_eq!(
        marks_of(&p.b, &p.need),
        Vec::<String>::new(),
        "the newer local unmark holds on B too"
    );
    assert_eq!(
        pulled_b.events_applied, 0,
        "the older letter applied nothing"
    );
}

#[test]
fn an_older_letter_arriving_after_a_newer_local_unmark_changes_nothing() {
    let p = pair();
    // B holds the mark, A marks unseen, then B unmarks: A's marked letter
    // is older than B's local unmark and arrives late.
    mark(&p.b, "mind", &p.need, true);
    mark(&p.a, "mind", &p.need, true);
    mark(&p.b, "mind", &p.need, false);

    let pulled = p.sync_a_to_b();

    assert_eq!(pulled.events_applied, 0, "{}", pulled.summary());
    assert_eq!(pulled.events_ignored, 0);
    assert_eq!(marks_of(&p.b, &p.need), Vec::<String>::new());
}

#[test]
fn a_held_foreign_row_follows_the_newest_mark_letter() {
    let w = World::new();
    // Bob's own work, so the mailspace is a normal one.
    w.bob
        .send_need("hand", &w.alice.address("mind"), "bob's own");
    // A need for carol's mailspace: bob holds it under carol's raw address.
    let carol = checkout("carol", &["mind"]);
    w.put_full(
        &wire_id(1, 0),
        "need",
        &carol.address("mind"),
        "for carol",
        "done_when: x\n",
        LETTER_DATE,
        "",
    );
    w.bob.pull(&w.bob_client);
    let item = handle(1);
    let row = w.bob.row(&w.bob.id_of(&item)).unwrap();
    assert!(is_foreign_account(&row.account), "{}", row.account);

    let id = format!("{}@x.test", wire_id(1, 0));
    let headers = format!(
        "In-Reply-To: <{id}>\r\nX-Vivi-Event: marked\r\nX-Vivi-Event-Of: <{id}>\r\n\
         X-Vivi-Event-Slug: {TRAVEL}\r\n"
    );
    w.put_full(
        &wire_id(2, 0),
        "mail",
        &w.bob.address("mind"),
        "Re: for carol",
        "marked",
        LETTER_DATE,
        &headers,
    );

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(marks_of(&w.bob, &item), vec![TRAVEL.to_string()]);
}
