//! A mark or a bind dated before an absorb counts whenever it arrives: with
//! the absorb in one pull, or after the absorb was already applied. The
//! folded marks and bound units are a function of the letters, so a member
//! that pulls everything at once lands where one that pulled them one by
//! one does — and where the server, which counts every mark and bind dated
//! before the seal, lands. A letter dated after the seal is ignored.
//!
//! The defect these lock out: with a seal held, the client sealed the item
//! and never applied the marks and binds dated before it.

use std::rc::Rc;
use std::thread::sleep;
use std::time::Duration;

use super::fake_server::{Checkout, FakeServer, checkout};
use crate::letter::fixture::body_for;
use crate::mailspace::{AbsorbConfig, RoleUpdate, SendRequest};
use crate::storage::BACKLOG_GRAPH_CODE;

const TRAVEL: &str = "travel";

/// A member whose `mind` seat is of kind `mind`: it may close, mark, bind
/// and absorb for everyone, so what it writes travels. The operator
/// ruling's quiet wait is off: the tests absorb at once.
fn member(name: &str) -> Checkout {
    let mut checkout = checkout(name, &["mind"]);
    checkout.mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
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

fn send(from: &Checkout, to: &[String], role: &str, kind: &str, subject: &str) -> String {
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

/// The writer, a member that pulls letter by letter, and a member that
/// pulls everything in one go, over one server. Both hold the unit and the
/// need as their own work.
struct Trio {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    c: Checkout,
    need: String,
    unit: String,
}

fn trio() -> Trio {
    let server = FakeServer::new();
    let (a, b, c) = (member("alice"), member("bob"), member("carol"));
    let to = vec![b.address("mind"), c.address("mind")];
    let need = send(&a, &to, "needs", "need", "lower me");
    let unit = send(&a, &to, "tasks", "task", "the unit");
    Trio {
        server,
        a,
        b,
        c,
        need,
        unit,
    }
}

impl Trio {
    fn push(&self, who: &Checkout) {
        who.push(&who.client(&self.server));
    }

    fn pull(&self, who: &Checkout) -> crate::sync::pull::PullReport {
        who.pull(&who.client(&self.server))
    }

    /// `a` binds the unit to the need.
    fn bind(&self) {
        self.a
            .mailspace
            .backlog_bind_units(&self.need, std::slice::from_ref(&self.unit))
            .unwrap();
        sleep(Duration::from_millis(2));
    }

    /// `a` marks the need.
    fn mark(&self, add: bool) {
        self.a
            .mailspace
            .set_marks("mind", &self.need, "need", add, &[TRAVEL.to_string()])
            .unwrap();
        sleep(Duration::from_millis(2));
    }

    /// `a` closes the need.
    fn close(&self) {
        self.a
            .mailspace
            .move_item_outcome("mind", &self.need, "done", None, "need done", None)
            .unwrap();
        sleep(Duration::from_millis(2));
    }

    /// `a` absorbs the need.
    fn absorb(&self) {
        self.a
            .mailspace
            .absorb("mind", &self.need, None, "need")
            .unwrap();
        sleep(Duration::from_millis(2));
    }
}

/// The unit source ids bound to `need`'s backlog node here, sorted.
fn bound_units(checkout: &Checkout, need: &str) -> Vec<String> {
    let storage = checkout.mailspace.storage().unwrap();
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

/// The folded marks of the item's own row, the way `need show` prints them.
fn marks_of(checkout: &Checkout, handle: &str) -> Vec<String> {
    let id = checkout.id_of(handle);
    checkout
        .mailspace
        .thread(handle, false, 50, 50)
        .unwrap()
        .into_iter()
        .find(|message| message.message_id == id)
        .unwrap_or_else(|| panic!("the item is not in its own thread"))
        .marks
}

/// The folder and seal of the item's own row.
fn row_of(checkout: &Checkout, handle: &str) -> (String, bool) {
    let row = checkout.row(&checkout.id_of(handle)).unwrap();
    (row.local_role, row.absorbed_at.is_some())
}

#[test]
fn marks_and_binds_dated_before_an_absorb_land_in_one_pull() {
    let t = trio();
    t.push(&t.a);
    t.pull(&t.c);

    // The writer binds, marks, closes and absorbs; `c` pulls after each
    // step, `b` only once, after all of it.
    t.bind();
    t.push(&t.a);
    t.pull(&t.c);
    t.mark(true);
    t.push(&t.a);
    t.pull(&t.c);
    t.close();
    t.push(&t.a);
    t.pull(&t.c);
    t.absorb();
    t.push(&t.a);
    t.pull(&t.c);

    let pulled = t.pull(&t.b);

    assert_eq!(
        bound_units(&t.c, &t.need),
        vec![t.unit.clone()],
        "the member that pulled one by one is the reference"
    );
    assert_eq!(marks_of(&t.c, &t.need), vec![TRAVEL.to_string()]);
    assert_eq!(
        bound_units(&t.b, &t.need),
        vec![t.unit.clone()],
        "the bind dated before the seal lands with it: {}",
        pulled.summary()
    );
    assert_eq!(
        marks_of(&t.b, &t.need),
        vec![TRAVEL.to_string()],
        "the mark dated before the seal lands with it"
    );
    assert_eq!(row_of(&t.b, &t.need), ("done".to_string(), true));
    assert_eq!(row_of(&t.c, &t.need), ("done".to_string(), true));
}

#[test]
fn a_mark_and_a_bind_dated_before_an_absorb_land_when_they_arrive_after_it() {
    let t = trio();
    t.push(&t.a);
    t.pull(&t.b);
    t.pull(&t.c);

    // The writer binds and marks, and its letters wait in its outbox. A
    // third member closes and absorbs the need meanwhile, so the seal is
    // dated after both and reaches `b` first.
    t.bind();
    t.mark(true);
    t.c.mailspace
        .move_item_outcome("mind", &t.need, "done", None, "need done", None)
        .unwrap();
    sleep(Duration::from_millis(2));
    t.c.mailspace.absorb("mind", &t.need, None, "need").unwrap();
    t.push(&t.c);
    t.pull(&t.b);
    assert_eq!(row_of(&t.b, &t.need), ("done".to_string(), true));
    assert!(bound_units(&t.b, &t.need).is_empty(), "not arrived yet");
    assert!(marks_of(&t.b, &t.need).is_empty(), "not arrived yet");

    t.push(&t.a);
    let pulled = t.pull(&t.b);

    assert_eq!(
        bound_units(&t.b, &t.need),
        vec![t.unit.clone()],
        "the older bind lands after the seal: {}",
        pulled.summary()
    );
    assert_eq!(marks_of(&t.b, &t.need), vec![TRAVEL.to_string()]);
    assert_eq!(row_of(&t.b, &t.need), ("done".to_string(), true));

    // Settled once: another pull changes nothing.
    t.pull(&t.b);
    assert_eq!(bound_units(&t.b, &t.need), vec![t.unit.clone()]);
    assert_eq!(marks_of(&t.b, &t.need), vec![TRAVEL.to_string()]);
}

#[test]
fn a_mark_dated_after_the_absorb_is_ignored() {
    let t = trio();
    t.push(&t.a);
    t.pull(&t.c);

    // The writer closes and absorbs; `c`, which has not seen the seal,
    // marks the need afterwards. `b`, fresh, pulls all of it at once.
    t.close();
    t.absorb();
    t.push(&t.a);
    t.c.mailspace
        .set_marks("mind", &t.need, "need", true, &[TRAVEL.to_string()])
        .unwrap();
    t.push(&t.c);

    let pulled = t.pull(&t.b);

    assert_eq!(row_of(&t.b, &t.need), ("done".to_string(), true));
    assert!(
        marks_of(&t.b, &t.need).is_empty(),
        "a mark dated after the seal moves nothing: {}",
        pulled.summary()
    );
}
