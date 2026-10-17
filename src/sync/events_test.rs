//! State as letters across mailspaces: the owner's close and reopen are
//! reply letters that travel through the fake project server and move the
//! assignee's rows and nodes; an assignee's report does not move the owner's.

#[path = "property_events_test.rs"]
mod property;

#[path = "property_bind_test.rs"]
mod property_bind;

#[path = "property_emit_test.rs"]
mod property_emit;

#[path = "property_marks_test.rs"]
mod property_marks;

use crate::mailspace::{AbsorbConfig, Mailspace, RoleUpdate, SendRequest};
use crate::storage::StoredMessageView;
use crate::sync::fake_server::{Checkout, FakeServer, World, checkout, wire_id};
use crate::sync::transport::{Envelope, Transport};

pub(super) const BODY: &str = "done_when: it works\n";

fn alice() -> Checkout {
    checkout("alice", &["mind"])
}

fn bob() -> Checkout {
    checkout("bob", &["mind", "hand"])
}

/// The same mailspace on a second machine: same id, empty store.
fn second_checkout(of: &Checkout) -> Checkout {
    let mut copy = checkout("alice", &["mind"]);
    copy.mailspace.config.id = of.mailspace.config.id.clone();
    copy.settings = crate::sync::SyncSettings::resolve(&copy.mailspace).unwrap();
    copy
}

fn send_task(from: &Checkout, to: &str, subject: &str) -> String {
    let result = from
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: BODY.into(),
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

fn node_state(mailspace: &Mailspace, handle: &str) -> String {
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.source_id == handle)
        .unwrap_or_else(|| panic!("no node for {handle}"))
        .state
}

pub(super) fn role_of(checkout: &Checkout, handle: &str) -> String {
    checkout.row(&checkout.id_of(handle)).unwrap().local_role
}

fn close(checkout: &Checkout, who: &str, handle: &str, note: Option<&str>) {
    checkout
        .mailspace
        .move_task(who, handle, "done", note, None, &[], &[])
        .unwrap();
}

fn reopen(checkout: &Checkout, who: &str, handle: &str) {
    checkout
        .mailspace
        .move_task(who, handle, "tasks", None, None, &[], &[])
        .unwrap();
}

pub(super) fn state(checkout: &Checkout, handle: &str) -> Option<String> {
    checkout
        .mailspace
        .item_event_state(handle)
        .unwrap()
        .map(|state| state.state)
}

fn rows_of(checkout: &Checkout) -> Vec<(String, String, String)> {
    let mut shape: Vec<(String, String, String)> = checkout
        .rows()
        .into_iter()
        .map(|row: StoredMessageView| (row.account, row.local_role, row.content_id))
        .collect();
    shape.sort();
    shape
}

fn rfc_id(bytes: &[u8]) -> String {
    mail_parser::MessageParser::default()
        .parse(bytes)
        .unwrap()
        .message_id()
        .unwrap()
        .to_string()
}

/// The bare addresses of a header's address list, in order.
fn addresses(list: Option<&mail_parser::Address<'_>>) -> Vec<String> {
    list.into_iter()
        .flat_map(mail_parser::Address::iter)
        .filter_map(|address| address.address.as_deref().map(str::to_string))
        .collect()
}

/// The `X-Vivi-Event-At` stamp of a letter's bytes, in Unix microseconds.
fn at_of(bytes: &[u8]) -> Option<i64> {
    let parsed = mail_parser::MessageParser::default().parse(bytes)?;
    for header in parsed.headers() {
        if !header.name().eq_ignore_ascii_case("X-Vivi-Event-At") {
            continue;
        }
        if let mail_parser::HeaderValue::Text(text) = header.value() {
            return chrono::DateTime::parse_from_rfc3339(text.trim())
                .ok()
                .map(|time| time.timestamp_micros());
        }
    }
    None
}

/// One pair of mailspaces with a task from alice's `mind` to bob's `hand`,
/// filed on both sides.
struct Pair {
    server: std::rc::Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    task: String,
}

fn pair() -> Pair {
    let server = FakeServer::new();
    let (mut a, b) = (alice(), bob());
    // The pairs close and absorb in one breath, so the operator ruling's
    // quiet wait is off here; absorb_quiet_test covers the wait itself.
    a.mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
    let task = send_task(&a, &b.address("hand"), "ship it");
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    assert_eq!(role_of(&b, &task), "tasks");
    Pair { server, a, b, task }
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

#[test]
fn the_owners_close_empties_the_assignees_queue_and_completes_the_node() {
    let p = pair();
    assert_eq!(node_state(&p.b.mailspace, &p.task), "open");
    assert_eq!(node_state(&p.a.mailspace, &p.task), "open");

    close(&p.a, "mind", &p.task, Some("accepted"));
    assert_eq!(node_state(&p.a.mailspace, &p.task), "done");
    assert_eq!(role_of(&p.a, &p.task), "done", "the sender's only copy");
    assert_eq!(state(&p.a, &p.task).as_deref(), Some("done"));
    let pulled = p.sync_a_to_b();

    assert_eq!(
        role_of(&p.b, &p.task),
        "done",
        "the assignee's queue empties"
    );
    assert_eq!(node_state(&p.b.mailspace, &p.task), "done");
    assert_eq!(state(&p.b, &p.task).as_deref(), Some("done"));
    assert_eq!(pulled.events_applied, 1);
    assert_eq!(pulled.events_ignored, 0);
    assert!(
        pulled.summary().contains("1 event(s) applied"),
        "{}",
        pulled.summary()
    );
    assert!(p.b.mailspace.list("hand", "tasks").unwrap().is_empty());
    let inbox = p.b.mailspace.list("hand", "inbox").unwrap();
    assert_eq!(inbox.len(), 1, "the event is a letter in the thread");
    assert!(inbox[0].read_state, "events file read");
    let thread = p.b.mailspace.thread(&p.task, false, 10, 10).unwrap();
    assert_eq!(thread.len(), 2, "{thread:#?}");
    assert!(
        thread.iter().any(|m| m.body.contains("accepted")),
        "{thread:#?}"
    );
}

#[test]
fn an_assignees_own_done_is_a_report_and_does_not_change_the_owner() {
    let p = pair();

    close(&p.b, "hand", &p.task, Some("done on my side"));
    let before = p.b.rows().len();
    let pulled = p.sync_b_to_a();

    assert_eq!(role_of(&p.b, &p.task), "done", "its own copy moved");
    assert_eq!(p.b.rows().len(), before, "no event letter, just the note");
    assert_eq!(pulled.events_applied, 0);
    assert_eq!(state(&p.a, &p.task), None, "no event exists");
    assert_eq!(
        node_state(&p.a.mailspace, &p.task),
        "open",
        "the owner's node"
    );
    assert_eq!(role_of(&p.a, &p.task), "sent", "the owner's copy");
    let thread = p.a.mailspace.thread(&p.task, false, 10, 10).unwrap();
    assert_eq!(thread.len(), 2, "the report is a reply: {thread:#?}");
    assert!(
        thread.iter().any(|m| m.body.contains("done on my side")),
        "{thread:#?}"
    );
}

#[test]
fn the_owner_closes_after_the_report_and_a_reopen_returns_the_item() {
    let p = pair();
    close(&p.b, "hand", &p.task, Some("done on my side"));
    p.sync_b_to_a();

    close(&p.a, "mind", &p.task, None);
    p.sync_a_to_b();
    assert_eq!(role_of(&p.b, &p.task), "done");
    assert_eq!(node_state(&p.a.mailspace, &p.task), "done");

    reopen(&p.a, "mind", &p.task);
    assert_eq!(role_of(&p.a, &p.task), "sent", "back to awaiting");
    assert_eq!(node_state(&p.a.mailspace, &p.task), "open");
    assert_eq!(state(&p.a, &p.task).as_deref(), Some("reopened"));
    p.sync_a_to_b();

    assert_eq!(
        role_of(&p.b, &p.task),
        "tasks",
        "even though it had closed it"
    );
    assert_eq!(node_state(&p.b.mailspace, &p.task), "open");
    assert_eq!(state(&p.b, &p.task).as_deref(), Some("reopened"));

    // The chain goes on: close again.
    close(&p.a, "mind", &p.task, None);
    p.sync_a_to_b();
    assert_eq!(role_of(&p.b, &p.task), "done");
    assert_eq!(state(&p.b, &p.task).as_deref(), Some("done"));
}

#[test]
fn pulling_again_and_auditing_a_converged_store_change_nothing() {
    let p = pair();
    close(&p.a, "mind", &p.task, None);
    p.sync_a_to_b();
    let (rows_a, rows_b) = (rows_of(&p.a), rows_of(&p.b));

    let again = p.sync_a_to_b();
    let audit_b = p.b.mailspace.backlog_audit(true).unwrap();
    let audit_a = p.a.mailspace.backlog_audit(true).unwrap();

    assert_eq!(again.fetched, 0);
    assert!(audit_b.findings.is_empty(), "{:?}", audit_b.findings);
    assert!(audit_a.findings.is_empty(), "{:?}", audit_a.findings);
    assert_eq!((rows_of(&p.a), rows_of(&p.b)), (rows_a, rows_b));
}

// Reversed by the authority rule: any member's prefix domain speaks for an
// item, so the third member's event applies instead of being ignored.
#[test]
fn an_event_from_a_third_members_prefix_domain_applies() {
    let p = pair();
    let item_id = rfc_id(&p.b.bytes_of(&p.task));
    let mallory = p.server.client("mallory", "deadbeef");
    let to = p.b.address("hand");
    let third_member = format!(
        "From: mind@deadbeef.local\r\nTo: {to}\r\nSubject: Re: ship it\r\n\
         Message-ID: <forged-1@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         In-Reply-To: <{item_id}>\r\nX-Vivi-Event: done\r\nX-Vivi-Event-Of: <{item_id}>\r\n\
         X-Vivi-Kind: mail\r\n\r\ndone\r\n"
    );
    mallory
        .append(
            &wire_id(77, 0),
            &Envelope {
                raw_eml: third_member.as_bytes(),
                to: &to,
                kind: "mail",
            },
        )
        .unwrap();

    let pulled = p.b.pull(&p.b.client(&p.server));

    assert_eq!(
        pulled.filed.get("inbox"),
        Some(&1),
        "still a letter in the thread"
    );
    assert_eq!(pulled.events_applied, 1);
    assert_eq!(pulled.events_ignored, 0);
    assert_eq!(role_of(&p.b, &p.task), "done");
    assert_eq!(node_state(&p.b.mailspace, &p.task), "done");
    assert_eq!(state(&p.b, &p.task).as_deref(), Some("done"));
}

#[test]
fn an_assignee_cannot_speak_for_the_owner_by_claiming_an_unverifiable_domain() {
    let p = pair();
    let item_id = rfc_id(&p.a.bytes_of(&p.task));
    let owner = p.a.address("mind");
    let forge = |from: &str, id: &str| {
        format!(
            "From: {from}\r\nTo: {owner}\r\nSubject: Re: ship it\r\n\
             Message-ID: <{id}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
             In-Reply-To: <{item_id}>\r\nX-Vivi-Event: done\r\nX-Vivi-Event-Of: <{item_id}>\r\n\
             X-Vivi-Kind: mail\r\n\r\ndone\r\n"
        )
        .into_bytes()
    };
    // The assignee's mailspace may write `local` or the owner's legacy name
    // as the domain; the server cannot verify either, so neither is believed.
    let legacy = format!("mind@{}", p.a.mailspace.legacy_domain());
    let client = p.b.client(&p.server);
    for (n, from) in ["mind@local", legacy.as_str()].into_iter().enumerate() {
        let bytes = forge(from, &format!("claim-{n}"));
        client
            .append(
                &wire_id(80 + n as u64, 0),
                &Envelope {
                    raw_eml: &bytes,
                    to: &owner,
                    kind: "mail",
                },
            )
            .unwrap();
    }

    let pulled = p.a.pull(&p.a.client(&p.server));

    assert_eq!(pulled.events_ignored, 2, "{}", pulled.summary());
    assert_eq!(pulled.events_applied, 0);
    assert_eq!(node_state(&p.a.mailspace, &p.task), "open");
    assert_eq!(role_of(&p.a, &p.task), "sent");
    assert_eq!(state(&p.a, &p.task), None);
}

#[test]
fn an_event_that_arrives_before_its_item_is_applied_when_the_item_arrives() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let ca = a.client(&server);
    let task = send_task(&a, &b.address("hand"), "ship it");
    close(&a, "mind", &task, None);
    // The event reaches the server first.
    let event_row = a
        .rows()
        .into_iter()
        .find(|row| row.subject.starts_with("Re:"))
        .unwrap();
    let event_bytes = a.bytes_of(&event_row.message_id);
    ca.append(
        &event_row.message_id,
        &Envelope {
            raw_eml: &event_bytes,
            to: &b.address("hand"),
            kind: "mail",
        },
    )
    .unwrap();
    let cb = b.client(&server);
    let first = b.pull(&cb);
    assert_eq!(first.filed.get("inbox"), Some(&1));
    assert!(b.mailspace.list("hand", "tasks").unwrap().is_empty());

    a.push(&ca);
    let second = b.pull(&cb);

    assert_eq!(second.filed.get("done"), None);
    assert_eq!(role_of(&b, &task), "done", "applied when the item arrived");
    assert_eq!(node_state(&b.mailspace, &task), "done");
    assert_eq!(second.events_applied, 1);
}

#[test]
fn a_second_checkout_of_the_owner_follows_the_close_and_resync_converges() {
    let p = pair();
    close(&p.a, "mind", &p.task, Some("accepted"));
    p.sync_a_to_b();
    let a2 = second_checkout(&p.a);
    let c2 = a2.client(&p.server);

    let pulled = a2.pull(&c2);

    assert_eq!(role_of(&a2, &p.task), "done", "the only copy follows");
    assert_eq!(node_state(&a2.mailspace, &p.task), "done");
    assert_eq!(state(&a2, &p.task).as_deref(), Some("done"));
    assert_eq!(pulled.events_ignored, 0, "the owner's own domain");

    // A fresh checkout of the assignee reproduces its state from the letters.
    let mut b3 = bob();
    b3.mailspace.config.id = p.b.mailspace.config.id.clone();
    b3.settings = crate::sync::SyncSettings::resolve(&b3.mailspace).unwrap();
    let cb3 = b3.client(&p.server);
    b3.pull(&cb3);
    assert_eq!(role_of(&b3, &p.task), "done");
    assert_eq!(node_state(&b3.mailspace, &p.task), "done");
    assert_eq!(rows_of(&b3).len(), rows_of(&p.b).len());
}

#[test]
fn a_stale_store_converges_on_resync_and_by_audit_repair() {
    let p = pair();
    close(&p.a, "mind", &p.task, None);
    p.sync_a_to_b();
    // Roll B back by hand, as a checkout copied from before the event is.
    let b_row = p.b.row(&p.b.id_of(&p.task)).unwrap();
    let storage = p.b.mailspace.storage().unwrap();
    storage
        .move_message_to_role(&b_row.account, &b_row.message_id, "tasks")
        .unwrap();
    drop(storage);
    p.b.mailspace.backlog_reopen_item(&p.task).unwrap();
    assert_eq!(role_of(&p.b, &p.task), "tasks");

    let audit = p.b.mailspace.backlog_audit(false).unwrap();
    assert!(
        audit.findings.iter().any(|f| f.class == "event_state"),
        "{:?}",
        audit.findings
    );
    let repaired = p.b.mailspace.backlog_audit(true).unwrap();
    assert!(repaired.findings.iter().all(|f| f.repaired));
    assert_eq!(role_of(&p.b, &p.task), "done");
    assert_eq!(node_state(&p.b.mailspace, &p.task), "done");
    let clean = p.b.mailspace.backlog_audit(true).unwrap();
    assert!(clean.findings.is_empty(), "{:?}", clean.findings);

    // And again by resync, which presents every held letter again.
    let storage = p.b.mailspace.storage().unwrap();
    storage
        .move_message_to_role(&b_row.account, &b_row.message_id, "tasks")
        .unwrap();
    drop(storage);
    let report = crate::sync::pull::pull_with(
        &p.b.mailspace,
        &p.b.settings,
        &p.b.client(&p.server),
        &crate::sync::pull::PullPlan {
            window: None,
            resync: true,
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(role_of(&p.b, &p.task), "done", "{}", report.summary());
}

#[test]
fn concurrent_closes_leave_a_fork_that_resolves_the_same_on_both_checkouts() {
    let server = FakeServer::new();
    let a = alice();
    let b = bob();
    let task = send_task(&a, &b.address("hand"), "ship it");
    a.push(&a.client(&server));
    let a2 = second_checkout(&a);
    a2.pull(&a2.client(&server));
    assert_eq!(role_of(&a2, &task), "sent");

    // Neither has seen the other's close.
    close(&a, "mind", &task, Some("from the first"));
    close(&a2, "mind", &task, Some("from the second"));
    a.push(&a.client(&server));
    a2.push(&a2.client(&server));
    a.pull(&a.client(&server));
    a2.pull(&a2.client(&server));

    let on_a = a.mailspace.item_event_state(&task).unwrap().unwrap();
    let on_a2 = a2.mailspace.item_event_state(&task).unwrap().unwrap();
    assert_eq!(on_a.heads, 2, "a fork");
    assert_eq!(on_a, on_a2, "the same head on both checkouts");

    // The next event names every head, so the fork is over.
    reopen(&a, "mind", &task);
    let healed = a.mailspace.item_event_state(&task).unwrap().unwrap();
    assert_eq!(healed.heads, 1);
    assert_eq!(healed.state, "reopened");
}

/// Letters put straight onto the server: an item from alice's `mind` for
/// bob's `mind`, and events about it with chosen ids, dates and headers.
fn event_headers(item: u64, kind: &str, prev: &[u64]) -> String {
    let item = format!("{}@x.test", wire_id(item, 0));
    let mut headers =
        format!("In-Reply-To: <{item}>\r\nX-Vivi-Event: {kind}\r\nX-Vivi-Event-Of: <{item}>\r\n");
    for id in prev {
        headers.push_str(&format!(
            "X-Vivi-Event-Prev: <{}@x.test>\r\n",
            wire_id(*id, 0)
        ));
    }
    headers
}

pub(super) fn put_event(w: &World, id: u64, item: u64, kind: &str, prev: &[u64], date: &str) {
    w.put_full(
        &wire_id(id, 0),
        "mail",
        &w.bob.address("mind"),
        "Re: item",
        kind,
        date,
        &event_headers(item, kind, prev),
    );
}

fn need_state(w: &World, item: u64) -> (String, String) {
    let handle = crate::sync::fake_server::handle(item);
    let row = w.bob.row(&w.bob.id_of(&handle)).unwrap();
    (row.local_role, node_state(&w.bob.mailspace, &handle))
}

// Reversed by the clock rule: the newest event is the later Date, even
// though the earlier-Date reopen names it as its Prev and is the chain head.
#[test]
fn a_later_date_wins_even_over_an_event_that_names_it_as_prev() {
    let w = World::new();
    w.put(1, "item", "done_when: x\n");
    // A done at noon with no Prev, then a reopen from a skewed clock an hour
    // earlier that names the done as its Prev.
    put_event(&w, 2, 1, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");
    put_event(
        &w,
        3,
        1,
        "reopened",
        &[2],
        "Sun, 11 Oct 2026 11:00:00 +0000",
    );

    w.bob.pull(&w.bob_client);

    assert_eq!(need_state(&w, 1), ("done".into(), "done".into()));
    let handle = crate::sync::fake_server::handle(1);
    let state = w.bob.mailspace.item_event_state(&handle).unwrap().unwrap();
    assert_eq!(state.state, "done");
    assert_eq!(state.heads, 1);
}

#[test]
fn a_fork_picks_the_later_date_then_the_larger_id_on_every_checkout() {
    let w = World::new();
    w.put(1, "item", "done_when: x\n");
    put_event(&w, 2, 1, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");
    put_event(&w, 3, 1, "reopened", &[], "Sun, 11 Oct 2026 12:00:01 +0000");
    // A third head at the same second as the second: the larger Message-ID.
    put_event(&w, 4, 1, "done", &[], "Sun, 11 Oct 2026 12:00:01 +0000");

    w.bob.pull(&w.bob_client);
    let handle = crate::sync::fake_server::handle(1);
    let first = w.bob.mailspace.item_event_state(&handle).unwrap().unwrap();

    let mut other = bob();
    other.mailspace.config.id = w.bob.mailspace.config.id.clone();
    let other_client = other.client(&w.server);
    other.pull(&other_client);
    let second = other.mailspace.item_event_state(&handle).unwrap().unwrap();

    assert_eq!(first.heads, 3);
    assert_eq!(first.state, "done", "the larger id of the two latest");
    assert_eq!(first, second);
    assert_eq!(need_state(&w, 1), ("done".into(), "done".into()));
}

/// [`put_event`] with an `X-Vivi-Event-At` stamp (RFC 3339).
fn put_stamped_event(
    w: &World,
    id: u64,
    item: u64,
    kind: &str,
    prev: &[u64],
    date: &str,
    at: &str,
) {
    let headers = format!(
        "{}X-Vivi-Event-At: {at}\r\n",
        event_headers(item, kind, prev)
    );
    w.put_full(
        &wire_id(id, 0),
        "mail",
        &w.bob.address("mind"),
        "Re: item",
        kind,
        date,
        &headers,
    );
}

#[test]
fn on_an_equal_date_the_successor_then_the_larger_id_wins_on_every_checkout() {
    let w = World::new();
    w.put(1, "item", "done_when: x\n");
    w.put(5, "item", "done_when: y\n");
    // Equal dates: the reopen names the done as its Prev, so the successor
    // wins although the done carries the larger Message-ID.
    put_event(&w, 3, 1, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");
    put_event(
        &w,
        2,
        1,
        "reopened",
        &[3],
        "Sun, 11 Oct 2026 12:00:00 +0000",
    );
    // With no Prev between two, the larger Message-ID wins.
    put_event(&w, 6, 5, "reopened", &[], "Sun, 11 Oct 2026 12:00:00 +0000");
    put_event(&w, 7, 5, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");

    w.bob.pull(&w.bob_client);

    assert_eq!(need_state(&w, 1), ("needs".into(), "open".into()));
    assert_eq!(need_state(&w, 5), ("done".into(), "done".into()));

    // Every checkout that holds the same letters agrees.
    let mut other = bob();
    other.mailspace.config.id = w.bob.mailspace.config.id.clone();
    let other_client = other.client(&w.server);
    other.pull(&other_client);
    for item in [1, 5] {
        let handle = crate::sync::fake_server::handle(item);
        assert_eq!(
            other.mailspace.item_event_state(&handle).unwrap().unwrap(),
            w.bob.mailspace.item_event_state(&handle).unwrap().unwrap(),
            "item {item}"
        );
    }
}

#[test]
fn an_event_at_stamp_beats_the_date_header() {
    let w = World::new();
    w.put(1, "item", "done_when: x\n");
    // The done is stamped after the reopen's later Date: the stamp decides,
    // and beats even the successor that names the done as its Prev.
    put_stamped_event(
        &w,
        2,
        1,
        "done",
        &[],
        "Sun, 11 Oct 2026 11:00:00 +0000",
        "2026-10-14T02:21:03.123456Z",
    );
    put_event(
        &w,
        3,
        1,
        "reopened",
        &[2],
        "Sun, 11 Oct 2026 12:00:00 +0000",
    );

    w.bob.pull(&w.bob_client);

    assert_eq!(need_state(&w, 1), ("done".into(), "done".into()));
    let handle = crate::sync::fake_server::handle(1);
    assert_eq!(
        w.bob
            .mailspace
            .item_event_state(&handle)
            .unwrap()
            .unwrap()
            .state,
        "done"
    );
}

#[test]
fn an_event_for_a_sealed_item_is_counted_and_not_applied() {
    let w = World::new();
    let id = w.put(1, "item", "done_when: x\n");
    w.bob.pull(&w.bob_client);
    let handle = crate::sync::fake_server::handle(1);
    w.bob
        .mailspace
        .absorb("mind", &handle, None, "need")
        .unwrap();
    assert!(!id.is_empty());
    put_event(&w, 2, 1, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.events_ignored, 1);
    assert_eq!(need_state(&w, 1).0, "needs", "the sealed item did not move");
}

#[test]
fn an_event_naming_no_held_item_changes_nothing() {
    let w = World::new();
    put_event(&w, 2, 9, "done", &[], "Sun, 11 Oct 2026 12:00:00 +0000");

    let pulled = w.bob.pull(&w.bob_client);

    assert_eq!(pulled.filed.get("inbox"), Some(&1));
    assert_eq!(pulled.events_applied + pulled.events_ignored, 0);
    assert_eq!(w.bob.rows().len(), 1);
}

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

#[test]
fn another_mind_in_the_owning_mailspace_closes_and_the_assignees_queue_empties() {
    let server = FakeServer::new();
    let mut a = alice();
    minds(&mut a, "lead");
    a.mailspace.add_identity("auditor").unwrap();
    let b = bob();
    let task = send_task(&a, &b.address("hand"), "ship it");
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    let p = Pair { server, a, b, task };

    // `lead` did not author the task and names `mind`'s sent handle: a mind
    // closes work for everyone. A role of another kind may not.
    let refused =
        p.a.mailspace
            .move_task("auditor", &p.task, "done", None, None, &[], &[])
            .unwrap_err()
            .to_string();
    assert!(refused.contains("held by identity 'mind'"), "{refused}");
    assert_eq!(role_of(&p.a, &p.task), "sent");

    let moved =
        p.a.mailspace
            .move_task_outcome("lead", &p.task, "done", Some("accepted"), None, &[], &[])
            .unwrap();
    assert!(moved.event.is_some());
    assert!(!moved.reported);
    assert_eq!(state(&p.a, &p.task).as_deref(), Some("done"));
    assert_eq!(node_state(&p.a.mailspace, &p.task), "done");
    let pulled = p.sync_a_to_b();

    assert_eq!(pulled.events_applied, 1);
    assert_eq!(pulled.events_ignored, 0);
    assert_eq!(role_of(&p.b, &p.task), "done", "the queue empties");
    assert_eq!(node_state(&p.b.mailspace, &p.task), "done");
    assert!(p.b.mailspace.list("hand", "tasks").unwrap().is_empty());

    // The same mind reopens it, and the item returns.
    p.a.mailspace
        .move_task("lead", &p.task, "tasks", None, None, &[], &[])
        .unwrap();
    p.sync_a_to_b();
    assert_eq!(role_of(&p.b, &p.task), "tasks");
    assert_eq!(state(&p.b, &p.task).as_deref(), Some("reopened"));
}

#[test]
fn a_mind_in_the_assignees_mailspace_closes_for_everyone_with_an_event_letter() {
    let server = FakeServer::new();
    let a = alice();
    let mut b = bob();
    minds(&mut b, "lead");
    let task = send_task(&a, &b.address("hand"), "ship it");
    a.push(&a.client(&server));
    b.pull(&b.client(&server));
    let p = Pair { server, a, b, task };

    // The mind did not author the item and its mailspace is not the
    // author's; the close is still an event letter, because no member owns
    // an item's state.
    let moved =
        p.b.mailspace
            .move_task_outcome("lead", &p.task, "done", Some("accepted"), None, &[], &[])
            .unwrap();
    assert!(moved.event.is_some());
    assert!(!moved.reported);
    assert_eq!(state(&p.b, &p.task).as_deref(), Some("done"));
    assert_eq!(node_state(&p.b.mailspace, &p.task), "done");

    // The letter is addressed to the item's From first — the author, who is
    // in another mailspace and is reached through sync — then its To.
    let letter = p.b.bytes_of(moved.event.as_deref().unwrap());
    let parsed = mail_parser::MessageParser::default()
        .parse(&letter)
        .unwrap();
    assert_eq!(
        addresses(parsed.to()),
        vec![p.a.address("mind"), p.b.address("hand")]
    );
    assert!(addresses(parsed.cc()).is_empty());
    assert!(at_of(&letter).is_some(), "every letter carries the stamp");

    let pulled = p.sync_b_to_a();

    assert_eq!(pulled.events_applied, 1);
    assert_eq!(pulled.events_ignored, 0);
    assert_eq!(role_of(&p.a, &p.task), "done", "the author's copy follows");
    assert_eq!(node_state(&p.a.mailspace, &p.task), "done");
    assert_eq!(state(&p.a, &p.task).as_deref(), Some("done"));
}

#[test]
fn an_event_letter_is_stamped_never_below_the_newest_held_event() {
    let mut w = World::new();
    minds(&mut w.bob, "lead");
    w.put(1, "item", "done_when: x\n");
    // A held event stamped five minutes ahead of the machine clock: far
    // enough that the clock alone would stamp below it, and inside the ten
    // minutes of skew a receiver still counts — a later stamp would be
    // ignored, not ordered, so it could not be a floor either.
    let held_at = chrono::DateTime::from_timestamp_micros(
        chrono::Utc::now().timestamp_micros() + 5 * 60 * 1_000_000,
    )
    .unwrap()
    .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    put_stamped_event(
        &w,
        2,
        1,
        "done",
        &[],
        "Sun, 11 Oct 2026 12:00:00 +0000",
        &held_at,
    );
    w.bob.pull(&w.bob_client);
    let handle = crate::sync::fake_server::handle(1);
    assert_eq!(state(&w.bob, &handle).as_deref(), Some("done"));

    // The mind reopens the item: the new letter carries a stamp, and the
    // stamp is at least one microsecond past the newest held event even
    // though the machine clock is five minutes earlier.
    let moved = w
        .bob
        .mailspace
        .move_item_outcome("lead", &handle, "needs", None, "need reopen", None)
        .unwrap();
    let event = moved.event.expect("the mind writes an event letter");
    let at = at_of(&w.bob.bytes_of(&event)).expect("every event letter carries X-Vivi-Event-At");
    let floor = chrono::DateTime::parse_from_rfc3339(&held_at)
        .unwrap()
        .timestamp_micros()
        + 1;
    assert!(at >= floor, "the stamp {at} is below the held event");
    assert_eq!(need_state(&w, 1).0, "needs", "the newer stamp wins here");
    assert_eq!(state(&w.bob, &handle).as_deref(), Some("reopened"));
}
