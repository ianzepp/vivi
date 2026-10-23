//! Two real mailspaces and one fake server: a letter composed on one side,
//! addressed to a role at the other's prefix, arrives with the same bytes,
//! id, handle, and hash, and the reply finds its way back.

use crate::letter::fixture::need_body;
use crate::mailspace::SendRequest;
use crate::storage::sha256_hex;
use crate::sync::fake_server::{Checkout, FakeServer, PROJECT, checkout};
use crate::sync::state::SyncState;
use crate::sync::sync_dir;
use crate::sync::transport::{Envelope, Transport};

fn alice() -> Checkout {
    checkout("alice", &["mind", "hand", "peer"])
}

fn bob() -> Checkout {
    checkout("bob", &["mind", "alice"])
}

#[test]
fn a_need_travels_as_its_exact_file() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (a.client(&server), b.client(&server));
    let handle = a.send_need("mind", &b.address("mind"), "ship the sync");
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
        Some(b.address("mind").as_str()),
        "the letter's own To"
    );
    assert_eq!(items[0].from, "alice");

    let pulled = b.pull(&cb);

    assert_eq!(pulled.fetched, 1);
    assert_eq!(pulled.filed.get("needs"), Some(&1));
    assert!(pulled.failure().is_none());
    assert!(pulled.warnings.is_empty(), "{:?}", pulled.warnings);
    let on_a = a.row(&wire_id).unwrap();
    let on_b = b.row(&wire_id).expect("same id on B");
    assert_eq!(on_b.content_id, on_a.content_id, "identical content id");
    assert_eq!(on_b.handle, handle, "same short handle");
    assert_eq!(on_b.local_role, "needs");
    assert_eq!(on_b.account, "mind", "the role the To address names");
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
    assert_eq!(on_a.local_role, "sent", "no local delivery on A");
    let pushed_row = on_a.remote.expect("the pushed row is bound");
    assert_eq!(pushed_row.provider, "vivi-project");
    assert_eq!(pushed_row.remote_uid, 1);
    assert_eq!(pushed_row.account, "mind", "the sent row's account");
    assert!(
        a.mailspace.backlog_item_tracked(&handle).unwrap(),
        "the sent copy is the backlog item on A"
    );
}

#[test]
fn replaying_push_and_pull_changes_nothing() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (a.client(&server), b.client(&server));
    a.send_need("mind", &b.address("mind"), "ship the sync");
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
    let (a, b) = (alice(), bob());
    let ca = a.client(&server);
    a.send_need("mind", &b.address("mind"), "mine");
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
    let (a, b) = (alice(), bob());
    let ca = a.client(&server);
    let to = b.address("mind");
    let handle = a.send_need("mind", &to, "pushed but unrecorded");
    let wire_id = a.id_of(&handle);
    // The push reached the server and the client died before it bound.
    ca.append(
        &wire_id,
        &Envelope {
            raw_eml: &a.bytes_of(&handle),
            to: &to,
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
fn a_reply_composed_on_b_with_no_flags_goes_back_to_the_sender_and_threads() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (a.client(&server), b.client(&server));
    let handle = a.send_need("mind", &b.address("mind"), "ship the sync");
    a.push(&ca);
    b.pull(&cb);

    let reply = b
        .mailspace
        .reply(
            &handle,
            "mind",
            Vec::new(),
            Vec::new(),
            None,
            "closed".into(),
        )
        .unwrap();

    assert!(reply.delivered.is_empty(), "nobody here to deliver to");
    assert_eq!(reply.remote, [a.address("mind")]);
    assert_eq!(
        b.mailspace.remote_line(&reply.remote).as_deref(),
        Some(format!("queued for sync: {}", a.address("mind")).as_str())
    );
    let pushed = b.push(&cb);
    assert_eq!(pushed.sent, 1, "{:?}", pushed.lines());
    let items = server.items();
    let reply_item = &items[1];
    assert_eq!(reply_item.from, "bob");
    assert_eq!(reply_item.to.as_deref(), Some(a.address("mind").as_str()));
    assert_eq!(reply_item.kind.as_deref(), Some("mail"));

    let pulled = a.pull(&ca);

    assert_eq!(pulled.filed.get("inbox"), Some(&1));
    assert_eq!(pulled.linked, 1);
    assert_eq!(pulled.existing, 1, "its own need");
    assert!(pulled.warnings.is_empty(), "{:?}", pulled.warnings);
    let thread = a.mailspace.thread(&handle, false, 10, 10).unwrap();
    assert_eq!(thread.len(), 2, "{thread:#?}");
    let inbox = a.mailspace.list("mind", "inbox").unwrap();
    assert_eq!(inbox.len(), 1);
    assert!(inbox[0].from_addr.starts_with("mind@"), "{inbox:?}");
    assert!(
        inbox[0]
            .from_addr
            .ends_with(&format!("{}.local", b.mailspace.address_prefix()))
    );
}

#[test]
fn a_letter_for_a_role_in_the_senders_own_mailspace_is_held_by_address_elsewhere() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (a.client(&server), b.client(&server));
    let handle = a.send_need("mind", "hand", "for a's own hand");
    a.push(&ca);

    let pulled = b.pull(&cb);

    assert_eq!(
        pulled.filed.get("needs"),
        Some(&1),
        "work is visible to all"
    );
    let row = b.row(&a.id_of(&handle)).unwrap();
    assert_eq!(row.account, a.address("hand"), "the first To, verbatim");
    assert_eq!(row.local_role, "needs");
    assert!(pulled.warnings.is_empty());
}

#[test]
fn a_send_to_a_local_role_and_a_remote_one_does_both() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let (ca, cb) = (a.client(&server), b.client(&server));

    let result = a
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into(), b.address("mind")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "both".into(),
            body: need_body(&["done"], "to both\n"),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap();

    assert_eq!(result.delivered.len(), 1);
    assert_eq!(result.delivered[0].identity, "hand");
    assert_eq!(result.remote, [b.address("mind")]);
    let delivered_id = a.id_of(&result.delivered[0].handle);
    let bytes = a.bytes_of(&result.delivered[0].handle);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains(&a.address("hand")), "{text}");
    assert!(text.contains(&b.address("mind")), "{text}");

    let pushed = a.push(&ca);
    assert_eq!(pushed.sent, 1);
    assert_eq!(server.items()[0].message_id, delivered_id);
    let mut to: Vec<String> = vec![a.address("hand"), b.address("mind")];
    to.sort();
    assert_eq!(server.items()[0].to.as_deref(), Some(to.join(",").as_str()));
    let pulled = b.pull(&cb);
    assert_eq!(pulled.filed.get("needs"), Some(&1));
    assert_eq!(b.row(&delivered_id).unwrap().account, "mind");
}

#[test]
fn a_restore_from_an_empty_store_reproduces_delivered_and_sent_rows() {
    let server = FakeServer::new();
    let a = alice();
    let ca = a.client(&server);
    let need = a.send_need("mind", "hand", "local to local");
    a.push(&ca);
    let original = a.rows();
    assert_eq!(original.len(), 2, "delivered and sent");

    // The same mailspace on a machine that lost its store: same id, no rows.
    let mut restored = alice();
    restored.mailspace.config.id = a.mailspace.config.id.clone();
    let cr = restored.client(&server);
    assert_eq!(restored.address("hand"), a.address("hand"));

    let pulled = restored.pull(&cr);

    assert_eq!(pulled.filed.get("needs"), Some(&1), "{:?}", pulled.filed);
    assert_eq!(pulled.filed.get("sent"), Some(&1), "{:?}", pulled.filed);
    let summary = |rows: Vec<crate::storage::StoredMessageView>| {
        let mut shape: Vec<(String, String, String)> = rows
            .into_iter()
            .map(|row| (row.account, row.local_role, row.content_id))
            .collect();
        shape.sort();
        shape
    };
    assert_eq!(summary(restored.rows()), summary(original));
    let delivered = restored.row(&a.id_of(&need)).unwrap();
    assert_eq!(delivered.local_role, "needs");
    assert_eq!(delivered.account, "hand");
    assert!(
        restored
            .mailspace
            .backlog_item_tracked(&delivered.handle)
            .unwrap()
    );
    let again = restored.push(&cr);
    assert_eq!(again.sent + again.duplicates, 0, "everything is bound");
}

#[test]
fn a_letter_forged_with_another_mailspaces_address_is_refused_end_to_end() {
    let server = FakeServer::new();
    let (a, b) = (alice(), bob());
    let mallory = server.client("mallory", "deadbeef");
    let legacy = server.legacy_client("oldtimer");
    let to = b.address("mind");
    let forge = |from: &str, subject: &str| {
        format!(
            "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\n\
             Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nb\r\n"
        )
        .into_bytes()
    };
    let posts = [
        (
            &mallory,
            "msg_00000000000000000000f001",
            forge(&b.address("hand"), "as-bob"),
        ),
        (
            &mallory,
            "msg_00000000000000000000f002",
            forge(&a.address("mind"), "as-alice"),
        ),
        (
            &legacy,
            "msg_00000000000000000000f003",
            forge(&a.address("mind"), "unstamped"),
        ),
        (
            &mallory,
            "msg_00000000000000000000f004",
            forge("mind@oldproj.local", "legacy-domain"),
        ),
    ];
    for (client, id, bytes) in &posts {
        append(*client, id, bytes, &to, "mail");
    }
    let honest = a.send_need("mind", &to, "the real thing");
    a.push(&a.client(&server));

    let pulled = b.pull(&b.client(&server));

    assert_eq!(pulled.fetched, 5);
    let refused: Vec<&str> = pulled
        .refused
        .iter()
        .map(|r| r.message_id.as_str())
        .collect();
    assert_eq!(
        refused,
        [
            "msg_00000000000000000000f001",
            "msg_00000000000000000000f002",
            "msg_00000000000000000000f003"
        ]
    );
    assert!(pulled.failure().is_some());
    assert_eq!(
        pulled.filed.get("inbox"),
        Some(&1),
        "the legacy-domain From"
    );
    assert_eq!(pulled.filed.get("needs"), Some(&1), "the honest need");
    assert_eq!(pulled.filed.get("sent"), None, "no forged sent row");
    assert!(b.row(&a.id_of(&honest)).is_some());
    assert_eq!(b.rows().len(), 2);
}

/// The need and a reply to it, composed on `source` for `reader`'s mind.
/// Returns `(need id, need bytes, reply id, reply bytes)`.
fn need_and_reply(source: &Checkout, reader: &Checkout) -> (String, Vec<u8>, String, Vec<u8>) {
    let to = reader.address("mind");
    let need = source.send_need("mind", &to, "topic");
    let reply = source.reply(&need, "mind", &to, "an answer");
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
    let reader = checkout("carol", &["mind"]);
    let (from_alice, to_carol) = (source.client(&server), reader.client(&server));
    let (need_id, need, reply_id, reply) = need_and_reply(&source, &reader);
    let to = reader.address("mind");

    append(&from_alice, &reply_id, &reply, &to, "mail");
    let first = reader.pull(&to_carol);

    assert_eq!(first.filed.get("inbox"), Some(&1));
    assert_eq!(first.linked, 0);
    let dir = sync_dir(&reader.mailspace.dir);
    let waiting = SyncState::load(&dir, PROJECT).unwrap();
    assert_eq!(waiting.dangling.len(), 1, "the orphan reply is remembered");
    assert_eq!(waiting.since, 1);

    append(&from_alice, &need_id, &need, &to, "need");
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
    let reader = checkout("carol", &["mind"]);
    let (from_alice, to_carol) = (source.client(&server), reader.client(&server));
    let (need_id, need, reply_id, reply) = need_and_reply(&source, &reader);
    let to = reader.address("mind");
    append(&from_alice, &reply_id, &reply, &to, "mail");
    append(&from_alice, &need_id, &need, &to, "need");

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
    let reader = checkout("carol", &["mind"]);
    let (from_alice, to_carol) = (source.client(&server), reader.client(&server));
    let (need_id, need, reply_id, reply) = need_and_reply(&source, &reader);
    let to = reader.address("mind");
    append(&from_alice, &reply_id, &reply, &to, "mail");
    reader.pull(&to_carol);
    // Lose the pull state: the cursor and the remembered orphan.
    std::fs::remove_file(sync_dir(&reader.mailspace.dir).join("state.json")).unwrap();
    append(&from_alice, &need_id, &need, &to, "need");
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
    let (ca, cb) = (a.client(&server), b.client(&server));
    let handles: Vec<String> = ["one", "two", "three"]
        .iter()
        .map(|subject| a.send_need("mind", &b.address("mind"), subject))
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
