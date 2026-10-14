//! The ruling proved end to end: two mailspaces against the loopback fake
//! hold one need, its state events travel like letters, and on every member
//! the state is what the newest event says — "when two machines disagree
//! about a need state, by definition, the last update or event created
//! against that need is controlling" (operator, 2026-10-14). No member owns
//! an item's state: a close on A reaches B, a reopen on B reaches A, two
//! unseen events settle on the newer one on both whatever the arrival
//! order, a seal stops later letters, and marks fold the same everywhere.

use std::rc::Rc;

use crate::mailspace::{ItemEventState, Mailspace, MoveOutcome, RoleUpdate, SendRequest};
use crate::sync::events::{role_of, state};
use crate::sync::fake_server::{Checkout, FakeServer, LETTER_DATE, checkout, letter, wire_id};
use crate::sync::push::PushReport;
use crate::sync::transport::{Envelope, Transport};

/// One hour in microseconds: the clear gap between two conflicting events.
const HOUR_US: i64 = 3_600 * 1_000_000;

fn alice() -> Checkout {
    checkout("alice", &["mind"])
}

/// B's `mind` is of kind `mind`: the voice that may close or reopen work
/// for everyone on any member — the ruling's "no member owns an item's
/// state" — the same for-everyone right `event_wire_test` grants its
/// cross-member mind.
fn bob() -> Checkout {
    let mut b = checkout("bob", &["mind", "hand"]);
    b.mailspace
        .set_role(
            "mind",
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    b
}

/// Two mailspaces and one need: A's `mind` files it (a local `needs` row,
/// so the work is A's too) and addresses it to B's `hand`, which holds its
/// own row and node after the first push and pull.
struct Pair {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    need: String,
}

fn pair() -> Pair {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let result = a
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![b.address("hand")],
            cc: vec![a.address("mind")],
            bcc: Vec::new(),
            subject: "prove the ruling".into(),
            body: "done_when: both mailspaces agree\n".into(),
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
    Pair { server, a, b, need }
}

impl Pair {
    fn a_to_b(&self) -> crate::sync::pull::PullReport {
        self.a.push(&self.a.client(&self.server));
        self.b.pull(&self.b.client(&self.server))
    }

    fn b_to_a(&self) -> crate::sync::pull::PullReport {
        self.b.push(&self.b.client(&self.server));
        self.a.pull(&self.a.client(&self.server))
    }
}

fn close_on(checkout: &Checkout, who: &str, handle: &str) -> MoveOutcome {
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
        .unwrap()
}

fn reopen_on(checkout: &Checkout, who: &str, handle: &str) {
    checkout
        .mailspace
        .move_item_outcome(who, handle, "needs", None, "need reopen", None)
        .unwrap();
}

fn node_state(mailspace: &Mailspace, handle: &str) -> String {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == handle)
        .unwrap_or_else(|| panic!("no backlog node for {handle}"))
        .state
}

fn item_state(checkout: &Checkout, handle: &str) -> ItemEventState {
    checkout
        .mailspace
        .item_event_state(handle)
        .unwrap()
        .expect("the item holds an authorized event")
}

/// The `X-Vivi-Event-At` stamp of a letter's bytes, in Unix microseconds.
fn event_at(bytes: &[u8]) -> i64 {
    let parsed = mail_parser::MessageParser::default()
        .parse(bytes)
        .expect("the letter parses");
    for header in parsed.headers() {
        if !header.name().eq_ignore_ascii_case("X-Vivi-Event-At") {
            continue;
        }
        if let mail_parser::HeaderValue::Text(text) = header.value() {
            return chrono::DateTime::parse_from_rfc3339(text.trim())
                .expect("the stamp is RFC 3339")
                .timestamp_micros();
        }
    }
    panic!("the letter carries no X-Vivi-Event-At");
}

/// An instant in Unix microseconds as RFC 3339 UTC with microseconds.
fn stamp(micros: i64) -> String {
    chrono::DateTime::from_timestamp_micros(micros)
        .expect("a representable instant")
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// The `Message-ID` of the item a handle names.
fn rfc_id(checkout: &Checkout, handle: &str) -> String {
    mail_parser::MessageParser::default()
        .parse(&checkout.bytes_of(handle))
        .expect("the item parses")
        .message_id()
        .expect("the item has a Message-ID")
        .to_string()
}

/// A hand-built event letter on the wire, the way its author's vivi pushes
/// one: from `author`'s mailspace (so the server stamps its prefix),
/// traveling as plain mail like every event letter, replying to the item,
/// stamped `at` — an offline writer whose clock reads `at`.
fn put_event_letter(
    server: &Rc<FakeServer>,
    author: &Checkout,
    to: &str,
    item_id: &str,
    kind: &str,
    at: i64,
    n: u64,
) {
    let id = wire_id(n, 0);
    let from = author.address("mind");
    let headers = format!(
        "In-Reply-To: <{item_id}>\r\nX-Vivi-Event: {kind}\r\nX-Vivi-Event-Of: <{item_id}>\r\n\
         X-Vivi-Event-At: {}\r\n",
        stamp(at)
    );
    let bytes = letter(
        &id,
        &from,
        to,
        "Re: prove the ruling",
        kind,
        LETTER_DATE,
        &headers,
    );
    author
        .client(server)
        .append(
            &id,
            &Envelope {
                raw_eml: &bytes,
                to,
                kind: "mail",
            },
        )
        .expect("the fake server takes the letter");
}

/// A push report with nothing refused, skipped, held back or stopped.
fn assert_clean(push: &PushReport) {
    assert!(push.refused.is_empty(), "{:?}", push.refused);
    assert!(push.unbound.is_empty(), "{:?}", push.unbound);
    assert!(push.skipped.is_empty(), "{:?}", push.skipped);
    assert!(push.stopped.is_none(), "{:?}", push.stopped);
    assert_eq!(push.held_back, 0);
}

/// The folded marks of the item's own row, the way `need show` prints them.
fn marks_of(checkout: &Checkout, item: &str) -> Vec<String> {
    let id = checkout.id_of(item);
    checkout
        .mailspace
        .thread(item, false, 50, 50)
        .unwrap()
        .into_iter()
        .find(|message| message.message_id == id)
        .expect("the item is in its own thread")
        .marks
}

#[test]
fn a_need_closed_on_a_is_done_on_b_after_a_pull() {
    let p = pair();
    assert_eq!(role_of(&p.b, &p.need), "needs");
    assert_eq!(node_state(&p.b.mailspace, &p.need), "open");

    close_on(&p.a, "mind", &p.need);
    assert_eq!(role_of(&p.a, &p.need), "done", "the closer's own row");
    let pushed = p.a.push(&p.a.client(&p.server));
    assert_clean(&pushed);
    assert!(pushed.sent >= 1, "{} letter(s) sent", pushed.sent);

    let pulled = p.a_to_b();

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(pulled.events_ignored, 0);
    assert_eq!(role_of(&p.b, &p.need), "done", "B's row is in done");
    assert_eq!(state(&p.b, &p.need).as_deref(), Some("done"));
    assert_eq!(node_state(&p.b.mailspace, &p.need), "done");
    // B's `need list --status done` equivalent: the done role holds the row.
    let done: Vec<String> =
        p.b.mailspace
            .list("hand", "done")
            .unwrap()
            .into_iter()
            .map(|row| row.message_id)
            .collect();
    assert!(done.contains(&p.b.id_of(&p.need)), "{done:?}");
}

#[test]
fn a_need_reopened_on_b_is_open_on_a_after_a_pull() {
    let p = pair();
    close_on(&p.a, "mind", &p.need);
    p.a_to_b();
    assert_eq!(role_of(&p.b, &p.need), "done");

    // B's mind reopens it: no member owns the state, so the reopen is an
    // event letter addressed to A, the author.
    reopen_on(&p.b, "mind", &p.need);
    assert_eq!(role_of(&p.b, &p.need), "needs");
    assert_eq!(state(&p.b, &p.need).as_deref(), Some("reopened"));
    let pushed = p.b.push(&p.b.client(&p.server));
    assert_clean(&pushed);

    let pulled = p.a.pull(&p.a.client(&p.server));

    assert_eq!(pulled.events_applied, 1, "{}", pulled.summary());
    assert_eq!(role_of(&p.a, &p.need), "needs", "A's row is back in needs");
    assert_eq!(node_state(&p.a.mailspace, &p.need), "open", "open again");
    assert_eq!(state(&p.a, &p.need).as_deref(), Some("reopened"));
}

/// What both mailspaces must show after the fork settles: the newer event's
/// state, the same head event, and the open rows and nodes it names.
fn assert_both_show_the_newer(p: &Pair) {
    let (on_a, on_b) = (item_state(&p.a, &p.need), item_state(&p.b, &p.need));
    assert_eq!(on_a, on_b, "A and B agree on the head");
    assert_eq!(on_a.state, "reopened", "the newer event's state");
    assert_eq!(on_a.heads, 2, "a fork settled by the clock, not the chain");
    assert_eq!(role_of(&p.a, &p.need), "needs");
    assert_eq!(role_of(&p.b, &p.need), "needs");
    assert_eq!(node_state(&p.a.mailspace, &p.need), "open");
    assert_eq!(node_state(&p.b.mailspace, &p.need), "open");
}

/// The stamp of the close A's mind just wrote, through A's own clock.
fn closed_at(p: &Pair) -> (i64, String) {
    let closed = close_on(&p.a, "mind", &p.need);
    let letter = closed
        .event
        .expect("the author's close writes an event letter");
    (event_at(&p.a.bytes_of(&letter)), rfc_id(&p.a, &p.need))
}

/// A reopen letter from B stamped a clear hour newer than `at`.
fn b_reopens_newer(p: &Pair, at: i64, n: u64) {
    put_event_letter(
        &p.server,
        &p.b,
        &p.a.address("mind"),
        &rfc_id(&p.a, &p.need),
        "reopened",
        at + HOUR_US,
        n,
    );
}

/// The fork with the newer event pulled last: B meets A's close first, then
/// both sides pull B's reopen.
fn newer_letter_pulled_last() -> Pair {
    let p = pair();
    let (at, _) = closed_at(&p);
    p.a.push(&p.a.client(&p.server));
    p.b.pull(&p.b.client(&p.server));
    b_reopens_newer(&p, at, 701);
    p.a.pull(&p.a.client(&p.server));
    p.b.pull(&p.b.client(&p.server));
    p
}

/// The same fork with the older event pulled last: B's reopen is on the
/// wire before A's close, and B pulls the close after it.
fn older_letter_pulled_last() -> Pair {
    let p = pair();
    let (at, _) = closed_at(&p);
    b_reopens_newer(&p, at, 702);
    p.b.pull(&p.b.client(&p.server));
    p.a.push(&p.a.client(&p.server));
    p.a.pull(&p.a.client(&p.server));
    p.b.pull(&p.b.client(&p.server));
    p
}

#[test]
fn two_conflicting_events_resolve_to_the_newer_on_both() {
    assert_both_show_the_newer(&newer_letter_pulled_last());
    // The arrival order reversed — the older event is the one pulled last —
    // and the ending is the same.
    assert_both_show_the_newer(&older_letter_pulled_last());
}

#[test]
fn an_absorb_on_a_seals_b_and_an_older_event_after_it_is_ignored() {
    let p = pair();
    close_on(&p.a, "mind", &p.need);
    p.a.mailspace.absorb("mind", &p.need, None, "need").unwrap();
    let seal =
        p.a.rows()
            .into_iter()
            .map(|row| p.a.bytes_of(&row.message_id))
            .find(|bytes| String::from_utf8_lossy(bytes).contains("X-Vivi-Event: absorbed"))
            .expect("the absorb wrote a letter");
    let seal_at = event_at(&seal);
    p.a.push(&p.a.client(&p.server));

    let sealed = p.b.pull(&p.b.client(&p.server));
    assert_eq!(sealed.events_ignored, 0, "{}", sealed.summary());
    let row = p.b.row(&p.b.id_of(&p.need)).unwrap();
    assert_eq!(row.local_role, "done", "sealed in done on B");
    assert!(row.absorbed_at.is_some(), "the seal closed the row");
    assert_eq!(node_state(&p.b.mailspace, &p.need), "done");
    assert_eq!(state(&p.b, &p.need).as_deref(), Some("done"));

    // A reopen older than the seal arrives on B afterwards: counted and
    // ignored, and the state does not change.
    put_event_letter(
        &p.server,
        &p.a,
        &p.b.address("hand"),
        &rfc_id(&p.a, &p.need),
        "reopened",
        seal_at - HOUR_US,
        703,
    );
    let ignored = p.b.pull(&p.b.client(&p.server));

    assert_eq!(ignored.events_applied, 0);
    assert_eq!(ignored.events_ignored, 1, "{}", ignored.summary());
    let row = p.b.row(&p.b.id_of(&p.need)).unwrap();
    assert_eq!(row.local_role, "done", "the seal holds");
    assert!(row.absorbed_at.is_some());
    assert_eq!(node_state(&p.b.mailspace, &p.need), "done");
    assert_eq!(state(&p.b, &p.need).as_deref(), Some("done"));
}

/// Marks travel because a mark or unmark by a role that may close the item
/// is a reply letter; a role that may not keeps a private row mark that
/// does not cross the wire. The bind is a graph fact of this mailspace in
/// the current base (no `bound` letter is written), so the mark letters are
/// what crosses the wire here.
#[test]
fn marks_travel_only_from_a_role_that_may_close() {
    let p = pair();

    // A mark on A (whose mind authored the need) is on B after a pull.
    p.a.mailspace
        .set_marks("mind", &p.need, "need", true, &["ruling".into()])
        .unwrap();
    assert_eq!(marks_of(&p.a, &p.need), vec!["ruling".to_string()]);
    let marked = p.a_to_b();
    assert_eq!(marked.events_applied, 1, "{}", marked.summary());
    assert_eq!(marks_of(&p.b, &p.need), vec!["ruling".to_string()]);

    // B's `hand` may not close the need, so its unmark is a private row
    // mark: no letter is written, and the mark does not travel — A still
    // carries it after both pulls.
    p.b.mailspace
        .set_marks("hand", &p.need, "need", false, &["ruling".into()])
        .unwrap();
    assert_eq!(marks_of(&p.b, &p.need), Vec::<String>::new());
    let unmarked = p.b_to_a();
    assert_eq!(unmarked.events_applied, 0, "{}", unmarked.summary());
    assert_eq!(
        marks_of(&p.a, &p.need),
        vec!["ruling".to_string()],
        "still on A"
    );
}
