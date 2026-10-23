//! An event letter dated more than ten minutes past the receiver's clock is
//! skipped when it arrives; it counts once the receiver's clock has passed
//! its bound, though no new letter arrives. The defect these lock out: the
//! bound was judged once at arrival, so `need show` — which folds again —
//! printed the newer state while the folders kept the old one, and no pull
//! or audit ever applied the letter.

use std::rc::Rc;

use super::fake_server::{Checkout, FakeServer, checkout};
use crate::letter::fixture::need_body;
use crate::mailspace::{RoleUpdate, SendRequest, with_clock_skew};
use crate::storage::BACKLOG_GRAPH_CODE;

const MINUTE: i64 = 60 * 1_000_000;
const TRAVEL: &str = "travel";

/// A member whose `mind` seat is of kind `mind`: it may close and mark work
/// for everyone, so what it writes travels.
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

/// Three members of one project server that hold one need: `a` authored it,
/// `b` and `c` hold it as their own work. `c`'s clock is the fast one.
struct Trio {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    c: Checkout,
    need: String,
}

fn trio() -> Trio {
    let server = FakeServer::new();
    let (a, b, c) = (member("alice"), member("bob"), member("carol"));
    let result = a
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![b.address("mind"), c.address("mind")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "skewed clocks".into(),
            body: need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let need = result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone());
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
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

    fn pull(&self, who: &Checkout) -> crate::sync::pull::PullReport {
        who.pull(&who.client(&self.server))
    }

    /// `c` closes the need with its clock `micros` ahead and pushes it.
    fn close_on_fast_c(&self, micros: i64) {
        with_clock_skew(micros, || {
            self.c
                .mailspace
                .move_item_outcome("mind", &self.need, "done", None, "need done", None)
                .unwrap();
        });
        self.push(&self.c);
    }

    /// `c` marks the need with its clock `micros` ahead and pushes it.
    fn mark_on_fast_c(&self, micros: i64) {
        with_clock_skew(micros, || {
            self.c
                .mailspace
                .set_marks("mind", &self.need, "need", true, &[TRAVEL.to_string()])
                .unwrap();
        });
        self.push(&self.c);
    }
}

/// The folder of the item's own row on `checkout`.
fn role_of(checkout: &Checkout, handle: &str) -> String {
    checkout.row(&checkout.id_of(handle)).unwrap().local_role
}

/// The state `need show` prints: the newest counted event, if any.
fn shown_state(checkout: &Checkout, handle: &str) -> Option<String> {
    checkout
        .mailspace
        .item_event_state(handle)
        .unwrap()
        .map(|state| state.state)
}

fn node_state(checkout: &Checkout, handle: &str) -> String {
    checkout
        .mailspace
        .graph_show(BACKLOG_GRAPH_CODE)
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == handle)
        .unwrap_or_else(|| panic!("no node for {handle}"))
        .state
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

#[test]
fn a_close_dated_past_the_clock_counts_at_the_next_pull_once_the_clock_passes() {
    let t = trio();
    t.close_on_fast_c(15 * MINUTE);

    let early = t.pull(&t.b);

    assert_eq!(early.events_applied, 0, "{}", early.summary());
    assert_eq!(
        early.events_ignored, 1,
        "the future close is counted ignored"
    );
    assert_eq!(role_of(&t.b, &t.need), "needs", "the folder stays open");
    assert_eq!(shown_state(&t.b, &t.need), None, "no counted event yet");

    // The clock moves on with no new letter: six minutes past, the close is
    // within ten minutes of it. The next pull applies it.
    let later = with_clock_skew(6 * MINUTE, || t.pull(&t.b));

    assert_eq!(later.events_applied, 1, "{}", later.summary());
    assert_eq!(later.events_ignored, 0, "nothing is counted twice");
    assert_eq!(role_of(&t.b, &t.need), "done", "the folder follows");
    assert_eq!(node_state(&t.b, &t.need), "done", "the node follows");
    // `need show` folds at the same clock: the folders agree with it.
    assert_eq!(
        with_clock_skew(6 * MINUTE, || shown_state(&t.b, &t.need)).as_deref(),
        Some("done"),
        "the folders agree with `need show`"
    );

    // Settled once: another pull changes nothing and reports nothing.
    let again = with_clock_skew(7 * MINUTE, || t.pull(&t.b));
    assert_eq!(again.events_applied, 0, "{}", again.summary());
    assert_eq!(again.events_ignored, 0, "{}", again.summary());
}

#[test]
fn every_member_ends_on_the_same_state_once_its_clock_passes_the_bound() {
    let t = trio();
    t.close_on_fast_c(15 * MINUTE);

    t.pull(&t.a);
    t.pull(&t.b);
    assert_eq!(role_of(&t.b, &t.need), "needs");
    assert_eq!(node_state(&t.a, &t.need), "open");

    // Each member's own clock passes the bound at its own time: `a` first,
    // `b` a pull later. `c`, whose clock wrote the letter, never skipped it.
    with_clock_skew(6 * MINUTE, || t.pull(&t.a));
    assert_eq!(node_state(&t.a, &t.need), "done");
    assert_eq!(role_of(&t.b, &t.need), "needs", "b's clock has not passed");

    with_clock_skew(8 * MINUTE, || t.pull(&t.b));
    assert_eq!(role_of(&t.b, &t.need), "done");
    assert_eq!(role_of(&t.c, &t.need), "done");
    for member in [&t.a, &t.b, &t.c] {
        assert_eq!(node_state(member, &t.need), "done");
        let shown = with_clock_skew(8 * MINUTE, || shown_state(member, &t.need));
        assert_eq!(shown.as_deref(), Some("done"));
    }
}

#[test]
fn a_close_still_past_the_bound_is_left_alone_and_not_recounted() {
    let t = trio();
    t.close_on_fast_c(40 * MINUTE);
    let first = t.pull(&t.b);
    assert_eq!(first.events_ignored, 1, "{}", first.summary());

    // Five minutes later the letter is still thirty-five minutes ahead.
    let second = with_clock_skew(5 * MINUTE, || t.pull(&t.b));

    assert_eq!(second.events_applied, 0, "{}", second.summary());
    assert_eq!(role_of(&t.b, &t.need), "needs");

    let past = with_clock_skew(31 * MINUTE, || t.pull(&t.b));
    assert_eq!(past.events_applied, 1, "{}", past.summary());
    assert_eq!(role_of(&t.b, &t.need), "done");
}

#[test]
fn a_mark_dated_past_the_clock_lands_at_the_next_pull_once_the_clock_passes() {
    let t = trio();
    t.mark_on_fast_c(15 * MINUTE);

    let early = t.pull(&t.b);
    assert_eq!(early.events_ignored, 1, "{}", early.summary());
    assert!(marks_of(&t.b, &t.need).is_empty(), "no mark yet");

    let later = with_clock_skew(6 * MINUTE, || t.pull(&t.b));

    assert_eq!(later.events_applied, 1, "{}", later.summary());
    assert_eq!(marks_of(&t.b, &t.need), vec![TRAVEL.to_string()]);
}

#[test]
fn the_audit_names_and_repairs_an_event_whose_bound_the_clock_has_passed() {
    let t = trio();
    t.close_on_fast_c(15 * MINUTE);
    t.pull(&t.b);

    // No pull runs: the audit alone sees the letter count, and says so.
    let found = with_clock_skew(6 * MINUTE, || t.b.mailspace.backlog_audit(false).unwrap());
    assert!(
        found
            .findings
            .iter()
            .any(|finding| finding.class == "event_state" && !finding.repaired),
        "{:?}",
        found.findings
    );
    assert_eq!(
        role_of(&t.b, &t.need),
        "needs",
        "a read-only audit moves nothing"
    );

    let repaired = with_clock_skew(6 * MINUTE, || t.b.mailspace.backlog_audit(true).unwrap());

    assert!(
        repaired
            .findings
            .iter()
            .any(|finding| finding.class == "event_state" && finding.repaired),
        "{:?}",
        repaired.findings
    );
    assert_eq!(role_of(&t.b, &t.need), "done");
    assert_eq!(node_state(&t.b, &t.need), "done");
}
