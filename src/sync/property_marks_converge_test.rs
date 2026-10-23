//! Marks converge across mailspaces: the folded marks of an item are a
//! function of its `marked`/`unmarked` letters alone, so three members that
//! hold the item reach the same folded marks whatever the order or the
//! number of their pulls. The defect these lock out: applying a letter
//! appends a local mark event at the receiver's clock, and that event used
//! to act as a floor that buried a later letter written between the two.

use std::rc::Rc;

use super::fake_server::{Checkout, FakeServer, checkout};
use crate::letter::fixture::need_body;
use crate::mailspace::{RoleUpdate, SendRequest};
use crate::sync::pull::PullReport;

const TRAVEL: &str = "travel";

fn alice() -> Checkout {
    member("alice")
}

fn bob() -> Checkout {
    member("bob")
}

fn carol() -> Checkout {
    member("carol")
}

/// A member whose `mind` seat is of kind `mind`, the way
/// `event_wire_test::minds` binds a role: only the item's author or a role
/// of that kind may close work for everyone, so a mark by any other seat
/// stays private and never travels.
fn member(name: &str) -> Checkout {
    let mut checkout = checkout(name, &["mind"]);
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
    checkout
}

/// Three members of one project server holding the same need, which alice
/// addressed to bob's and carol's `mind` so every member holds it as own
/// work and may mark it. Carol has pulled the need; bob has not.
struct Trio {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    c: Checkout,
    need: String,
}

fn trio() -> Trio {
    let server = FakeServer::new();
    let (a, b, c) = (alice(), bob(), carol());
    let result = a
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![b.address("mind"), c.address("mind")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "travel policy".into(),
            body: need_body(&["done"], "travel policy body\n"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();
    let need = result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone());
    a.push(&a.client(&server));
    c.pull(&c.client(&server));
    Trio {
        server,
        a,
        b,
        c,
        need,
    }
}

impl Trio {
    fn push(&self, who: &Checkout) {
        who.push(&who.client(&self.server));
    }

    fn pull(&self, who: &Checkout) -> PullReport {
        who.pull(&who.client(&self.server))
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

/// The review's sequence: A marks (letter L1); C pulls L1 and unmarks
/// (letter L2, newer than L1); B pulls L1 alone — applying it appends a
/// local mark event at B's clock — then pulls L2; A pulls L2.
fn review_sequence(t: &Trio) {
    mark(&t.a, "mind", &t.need, true);
    t.push(&t.a);
    t.pull(&t.c);
    mark(&t.c, "mind", &t.need, false);
    t.pull(&t.b);
    t.push(&t.c);
    t.pull(&t.b);
    t.pull(&t.a);
}

#[test]
fn three_members_converge_when_b_pulls_each_letter_separately() {
    let t = trio();
    review_sequence(&t);
    assert_eq!(marks_of(&t.a, &t.need), Vec::<String>::new(), "A unmarked");
    assert_eq!(
        marks_of(&t.b, &t.need),
        Vec::<String>::new(),
        "B applied L1, then must follow the newer L2"
    );
    assert_eq!(marks_of(&t.c, &t.need), Vec::<String>::new(), "C unmarked");
}

#[test]
fn a_fresh_member_pulling_both_letters_in_one_pull_lands_unmarked() {
    let t = trio();
    mark(&t.a, "mind", &t.need, true);
    t.push(&t.a);
    t.pull(&t.c);
    mark(&t.c, "mind", &t.need, false);
    t.push(&t.c);
    t.pull(&t.b);
    t.pull(&t.a);
    assert_eq!(
        marks_of(&t.b, &t.need),
        Vec::<String>::new(),
        "the newest letter controls in one pull"
    );
    assert_eq!(marks_of(&t.a, &t.need), Vec::<String>::new());
    assert_eq!(marks_of(&t.c, &t.need), Vec::<String>::new());
}

#[test]
fn reverse_arrival_order_still_lands_unmarked() {
    let t = trio();
    // A marks (the oldest marked letter) but does not push it yet; C marks
    // and unmarks of her own, so the unmark letter is on the wire first. B
    // receives the unmark, then the older marked letter arrives.
    mark(&t.a, "mind", &t.need, true);
    mark(&t.c, "mind", &t.need, true);
    t.push(&t.c);
    mark(&t.c, "mind", &t.need, false);
    t.push(&t.c);
    t.pull(&t.b);
    assert_eq!(marks_of(&t.b, &t.need), Vec::<String>::new());
    t.push(&t.a);
    t.pull(&t.b);
    t.pull(&t.a);
    assert_eq!(
        marks_of(&t.b, &t.need),
        Vec::<String>::new(),
        "the older marked letter arriving last does not win"
    );
    assert_eq!(marks_of(&t.a, &t.need), Vec::<String>::new());
    assert_eq!(marks_of(&t.c, &t.need), Vec::<String>::new());
}

#[test]
fn after_converging_a_new_mark_by_b_is_the_newest_and_travels() {
    let t = trio();
    review_sequence(&t);
    assert_eq!(marks_of(&t.b, &t.need), Vec::<String>::new());

    mark(&t.b, "mind", &t.need, true);
    t.push(&t.b);
    t.pull(&t.a);
    t.pull(&t.c);
    assert_eq!(marks_of(&t.a, &t.need), vec![TRAVEL.to_string()]);
    assert_eq!(marks_of(&t.b, &t.need), vec![TRAVEL.to_string()]);
    assert_eq!(marks_of(&t.c, &t.need), vec![TRAVEL.to_string()]);
}
