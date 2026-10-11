//! Two real mailspaces and one fake server: a letter composed on one side
//! arrives on the other with the same bytes, id, handle, and hash.

use crate::storage::sha256_hex;
use crate::sync::fake_server::{Checkout, FakeServer, PROJECT, checkout};
use crate::sync::state::SyncState;
use crate::sync::sync_dir;
use crate::sync::transport::{Envelope, Transport};

fn alice() -> Checkout {
    checkout(
        "alice",
        "mind",
        &["mind", "hand", "peer"],
        &[("peer", "bob")],
    )
}

fn bob() -> Checkout {
    checkout("bob", "mind", &["mind", "alice"], &[("alice", "alice")])
}

#[test]
fn a_need_travels_as_its_exact_file() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (server.client("alice"), server.client("bob"));
    let handle = a.send_need("mind", "hand", "ship the sync");
    let wire_id = a.id_of(&handle);

    let pushed = a.push(&ca);

    assert_eq!(pushed.sent, 1);
    assert!(pushed.failure().is_none(), "{:?}", pushed.lines());
    let items = server.items();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].message_id, wire_id);
    assert_eq!(
        items[0].bytes,
        a.bytes_of(&handle),
        "the stored file, as-is"
    );
    assert_eq!(items[0].kind.as_deref(), Some("need"));
    assert_eq!(
        items[0].to.as_deref(),
        Some("alice"),
        "unmapped role: own name"
    );
    assert_eq!(items[0].from, "alice");

    let pulled = b.pull(&cb);

    assert_eq!(pulled.fetched, 1);
    assert_eq!(pulled.filed.get("needs"), Some(&1));
    assert!(pulled.failure().is_none());
    let on_a = a.row(&wire_id).unwrap();
    let on_b = b.row(&wire_id).expect("same id on B");
    assert_eq!(on_b.content_id, on_a.content_id, "identical content id");
    assert_eq!(on_b.handle, handle, "same short handle");
    assert_eq!(on_b.local_role, "needs");
    assert_eq!(on_b.account, "mind", "filed under B's recipient");
    assert!(!on_b.read_state);
    assert!(b.mailspace.backlog_item_tracked(&on_b.handle).unwrap());
    assert_eq!(b.blob_hash(&on_b), a.blob_hash(&on_a));
    assert_eq!(b.blob_hash(&on_b), items[0].sha);
    assert_eq!(items[0].sha, on_a.content_id);

    let binding = on_b.remote.expect("pulled rows are bound");
    assert_eq!(binding.provider, "vivi-project");
    assert_eq!(binding.remote_mailbox, PROJECT);
    assert_eq!(binding.remote_uid, 1);
    assert_eq!(binding.remote_uidvalidity, 1);
    let pushed_row = a
        .row(&wire_id)
        .unwrap()
        .remote
        .expect("the pushed row is bound");
    assert_eq!(pushed_row.provider, "vivi-project");
    assert_eq!(pushed_row.remote_uid, 1);
    assert_eq!(pushed_row.account, "hand", "the delivered row's account");
}

#[test]
fn replaying_push_and_pull_changes_nothing() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (server.client("alice"), server.client("bob"));
    a.send_need("mind", "hand", "ship the sync");
    a.push(&ca);
    b.pull(&cb);
    let (rows_a, rows_b) = (a.rows(), b.rows());
    let (blobs_a, blobs_b) = (a.blob_files(), b.blob_files());
    let calls = server.append_calls();

    let push_again = a.push(&ca);
    let pull_again = b.pull(&cb);

    assert_eq!(push_again.sent + push_again.duplicates, 0);
    assert!(push_again.skipped.is_empty());
    assert_eq!(server.append_calls(), calls, "nothing was re-sent");
    assert_eq!(pull_again.fetched, 0);
    assert!(pull_again.filed.is_empty());
    assert_eq!(pull_again.existing, 0);
    assert_eq!(server.items().len(), 1);
    assert_eq!((a.rows(), b.rows()), (rows_a, rows_b));
    assert_eq!((a.blob_files(), b.blob_files()), (blobs_a, blobs_b));
}

#[test]
fn the_sender_pulling_its_own_item_back_makes_no_second_row() {
    let server = FakeServer::new();
    let a = alice();
    let ca = server.client("alice");
    a.send_need("mind", "hand", "mine");
    a.push(&ca);
    let rows = a.rows();
    let blobs = a.blob_files();

    let own = a.pull(&ca);

    assert_eq!(own.fetched, 1);
    assert!(own.filed.is_empty());
    assert_eq!(own.existing, 1);
    assert!(own.refused.is_empty(), "no conflict");
    assert_eq!(a.rows(), rows);
    assert_eq!(a.blob_files(), blobs);
}

#[test]
fn a_held_letter_the_push_never_recorded_is_bound_by_the_pull() {
    let server = FakeServer::new();
    let a = alice();
    let ca = server.client("alice");
    let handle = a.send_need("mind", "hand", "pushed but unrecorded");
    let wire_id = a.id_of(&handle);
    // The push reached the server and the client died before it bound.
    ca.append(
        &wire_id,
        &Envelope {
            raw_eml: &a.bytes_of(&handle),
            to: "alice",
            kind: "need",
        },
    )
    .unwrap();
    assert!(a.row(&wire_id).unwrap().remote.is_none());

    let pulled = a.pull(&ca);

    assert_eq!(pulled.existing, 1);
    let bound = a.row(&wire_id).unwrap().remote.expect("bound by the pull");
    assert_eq!(bound.remote_uid, 1);
    let calls = server.append_calls();
    let again = a.push(&ca);
    assert_eq!(again.sent + again.duplicates, 0);
    assert_eq!(server.append_calls(), calls, "not sent a second time");
}

#[test]
fn a_reply_composed_on_b_threads_with_the_need_on_a() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (server.client("alice"), server.client("bob"));
    let handle = a.send_need("mind", "peer", "ship the sync");
    a.push(&ca);
    b.pull(&cb);

    b.reply(&handle, "mind", "alice", "closed, thanks");
    let pushed = b.push(&cb);

    assert_eq!(pushed.sent, 1, "{:?}", pushed.lines());
    let reply = server.items().into_iter().nth(1).unwrap();
    assert_eq!(reply.from, "bob");
    assert_eq!(
        reply.to.as_deref(),
        Some("alice"),
        "mapped through [sync.names]"
    );
    assert_eq!(reply.kind.as_deref(), Some("mail"));

    let pulled = a.pull(&ca);

    assert_eq!(pulled.filed.get("inbox"), Some(&1));
    assert_eq!(pulled.linked, 1);
    assert_eq!(pulled.existing, 1, "its own need");
    let thread = a.mailspace.thread(&handle, false, 10, 10).unwrap();
    assert_eq!(thread.len(), 2, "{thread:#?}");
}

/// The need and a reply to it, composed on a source checkout. Returns
/// `(need id, need bytes, reply id, reply bytes)`.
fn need_and_reply(source: &Checkout) -> (String, Vec<u8>, String, Vec<u8>) {
    let need = source.send_need("mind", "hand", "topic");
    let reply = source.reply(&need, "hand", "mind", "an answer");
    (
        source.id_of(&need),
        source.bytes_of(&need),
        source.id_of(&reply),
        source.bytes_of(&reply),
    )
}

fn append(client: &impl Transport, id: &str, bytes: &[u8], to: &str, kind: &str) {
    let envelope = Envelope {
        raw_eml: bytes,
        to,
        kind,
    };
    client.append(id, &envelope).unwrap();
}

#[test]
fn a_reply_pulled_before_its_parent_is_linked_when_the_parent_arrives() {
    let server = FakeServer::new();
    let source = alice();
    let reader = checkout("carol", "mind", &["mind"], &[]);
    let (from_alice, to_carol) = (server.client("alice"), server.client("carol"));
    let (need_id, need, reply_id, reply) = need_and_reply(&source);

    append(&from_alice, &reply_id, &reply, "carol", "mail");
    let first = reader.pull(&to_carol);

    assert_eq!(first.filed.get("inbox"), Some(&1));
    assert_eq!(first.linked, 0);
    let dir = sync_dir(&reader.mailspace.dir);
    let waiting = SyncState::load(&dir, PROJECT).unwrap();
    assert_eq!(waiting.dangling.len(), 1, "the orphan reply is remembered");
    assert_eq!(waiting.since, 1);

    append(&from_alice, &need_id, &need, "alice", "need");
    let second = reader.pull(&to_carol);

    assert_eq!(second.filed.get("needs"), Some(&1));
    assert_eq!(second.linked, 1);
    assert!(SyncState::load(&dir, PROJECT).unwrap().dangling.is_empty());
    let need_handle = reader.row(&need_id).unwrap().handle;
    let thread = reader
        .mailspace
        .thread(&need_handle, false, 10, 10)
        .unwrap();
    assert_eq!(thread.len(), 2);
}

#[test]
fn a_reply_and_its_parent_in_one_page_are_linked_whatever_their_order() {
    let server = FakeServer::new();
    let source = alice();
    let reader = checkout("carol", "mind", &["mind"], &[]);
    let (from_alice, to_carol) = (server.client("alice"), server.client("carol"));
    let (need_id, need, reply_id, reply) = need_and_reply(&source);
    append(&from_alice, &reply_id, &reply, "carol", "mail");
    append(&from_alice, &need_id, &need, "alice", "need");

    let pulled = reader.pull(&to_carol);

    assert_eq!(pulled.fetched, 2);
    assert_eq!(pulled.linked, 1);
    assert!(
        SyncState::load(&sync_dir(&reader.mailspace.dir), PROJECT)
            .unwrap()
            .dangling
            .is_empty()
    );
}

#[test]
fn a_held_reply_without_a_link_is_linked_when_it_is_presented_again() {
    let server = FakeServer::new();
    let source = alice();
    let reader = checkout("carol", "mind", &["mind"], &[]);
    let (from_alice, to_carol) = (server.client("alice"), server.client("carol"));
    let (need_id, need, reply_id, reply) = need_and_reply(&source);
    append(&from_alice, &reply_id, &reply, "carol", "mail");
    reader.pull(&to_carol);
    // Lose the pull state: the cursor and the remembered orphan.
    std::fs::remove_file(sync_dir(&reader.mailspace.dir).join("state.json")).unwrap();
    append(&from_alice, &need_id, &need, "alice", "need");
    // Present the need first, in an earlier page than the reply.
    server.page_limit(1);

    let pulled = reader.pull(&to_carol);

    assert_eq!(pulled.fetched, 2);
    assert_eq!(pulled.existing, 1, "the reply was already held");
    assert_eq!(pulled.filed.get("needs"), Some(&1));
    assert_eq!(pulled.linked, 1, "linked although it was not new");
    assert_eq!(reader.rows().len(), 2);
}

#[test]
fn a_tampered_record_is_refused_and_the_rest_of_the_page_is_filed() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (server.client("alice"), server.client("bob"));
    let handles: Vec<String> = ["one", "two", "three"]
        .iter()
        .map(|subject| a.send_need("mind", "hand", subject))
        .collect();
    a.push(&ca);
    let victim = a.id_of(&handles[1]);
    server.tamper(&victim, b"From: evil\r\n\r\nnot the letter");

    let pulled = b.pull(&cb);

    assert_eq!(pulled.fetched, 3);
    assert_eq!(pulled.filed.get("needs"), Some(&2));
    assert_eq!(pulled.refused.len(), 1);
    assert_eq!(pulled.refused[0].message_id, victim);
    assert!(pulled.summary().contains(&victim), "{}", pulled.summary());
    assert!(pulled.failure().is_some());
    assert!(
        b.row(&victim).is_none(),
        "the tampered bytes are not ingested"
    );
    assert_eq!(b.blob_files().len(), 2, "and no blob was written for them");
    assert!(
        !b.blob_files()
            .iter()
            .any(|file| file.contains(&sha256_hex(b"From: evil\r\n\r\nnot the letter")))
    );
    assert_eq!(pulled.cursor, 3, "the page was handled");
}
