//! Letters a pull files wake `watch` exactly as locally delivered ones do.

use crate::mailspace::{Mailspace, MailspaceWatchRequest, SendRequest, run_watch};
use crate::sync::fake_server::{Checkout, FakeServer, World, checkout, handle, wire_id};
use crate::sync::fetch::fetch_handles;

fn request(role: &str, kinds: &str, events: &str) -> MailspaceWatchRequest {
    MailspaceWatchRequest {
        for_identity: role.into(),
        kinds: kinds.into(),
        events: events.into(),
        statuses: None,
        match_from: None,
        match_subject_prefix: None,
        handle: None,
        until_count: 1,
        // A zero timeout makes one scan and then fails: Ok means a match.
        timeout: Some("0s".into()),
        once: false,
        since: None,
        cursor_file: None,
        write_cursor: false,
        poll_interval: "10ms".into(),
        json: false,
    }
}

fn wakes(mailspace: &Mailspace, role: &str, kinds: &str) -> bool {
    run_watch(mailspace, request(role, kinds, "delivered,moved")).is_ok()
}

/// Only `moved` events: the delivery alone does not count.
fn wakes_moved(mailspace: &Mailspace, role: &str, kinds: &str) -> bool {
    run_watch(mailspace, request(role, kinds, "moved")).is_ok()
}

#[test]
fn pulled_letters_wake_the_watcher_for_their_kind_and_their_role() {
    let w = World::new();
    w.put(1, "a need", "body");
    let hand = w.bob.address("hand");
    w.put_full(
        &wire_id(2, 0),
        "task",
        &hand,
        "a task",
        "body",
        "Sun, 11 Oct 2026 12:00:00 +0000",
        "",
    );
    let mind = w.bob.address("mind");
    w.put_full(
        &wire_id(3, 0),
        "mail",
        &mind,
        "a note",
        "body",
        "Sun, 11 Oct 2026 12:00:00 +0000",
        "",
    );
    let before = w.bob.mailspace.storage().unwrap();
    assert!(
        before.list_mailspace_events_after(0).unwrap().is_empty(),
        "nothing is filed before the pull"
    );
    drop(before);
    assert!(!wakes(&w.bob.mailspace, "mind", "need"));

    w.bob.pull(&w.bob_client);

    let bob = &w.bob.mailspace;
    assert!(wakes(bob, "mind", "need"));
    assert!(!wakes(bob, "hand", "need"), "a need for another role");
    assert!(wakes(bob, "hand", "task"));
    assert!(!wakes(bob, "mind", "task"));
    assert!(wakes(bob, "mind", "mail"));
    assert!(!wakes(bob, "hand", "mail"));
    assert!(!wakes(bob, "hand", "want"));
}

#[test]
fn a_pulled_sent_copy_does_not_wake_the_senders_watcher() {
    let server = FakeServer::new();
    let a = checkout("alice", &["mind", "hand"]);
    let ca = a.client(&server);
    a.send_need("mind", "hand", "local to local");
    a.push(&ca);

    // The same mailspace on a machine that lost its store.
    let mut restored: Checkout = checkout("alice", &["mind", "hand"]);
    restored.mailspace.config.id = a.mailspace.config.id.clone();
    let pulled = restored.pull(&restored.client(&server));
    assert_eq!(pulled.filed.get("needs"), Some(&1), "{:?}", pulled.filed);
    assert_eq!(pulled.filed.get("sent"), Some(&1), "{:?}", pulled.filed);

    let kinds = "mail,task,need,want";
    assert!(wakes(&restored.mailspace, "hand", "need"));
    assert!(
        !wakes(&restored.mailspace, "mind", kinds),
        "the sender's own copy wakes nobody"
    );
}

#[test]
fn a_restored_letter_does_not_wake_a_watcher_a_second_time() {
    let w = World::new();
    w.put(1, "evict me", "body");
    w.bob.pull(&w.bob_client);
    let bob = &w.bob.mailspace;
    let cursor = w.bob.mailspace.dir.join("watch.cursor");
    let mut first = request("mind", "need", "delivered,moved");
    first.cursor_file = Some(cursor.clone());
    first.write_cursor = true;
    run_watch(bob, first).expect("the pulled need wakes the watcher");

    let evicted = bob
        .storage()
        .unwrap()
        .evict_messages(&[wire_id(1, 0)])
        .unwrap();
    assert_eq!(evicted, 1);
    let restored = fetch_handles(bob, &w.bob.settings, &w.bob_client, &[handle(1)]).unwrap();
    assert_eq!(restored.pull.restored, 1);

    let mut second = request("mind", "need", "delivered,moved");
    second.cursor_file = Some(cursor);
    assert!(
        run_watch(bob, second).is_err(),
        "a restore is not a new delivery"
    );
    let events = bob
        .storage()
        .unwrap()
        .list_mailspace_events(&wire_id(1, 0))
        .unwrap();
    assert_eq!(events.len(), 1, "{events:?}");
}

/// An event a pull applies — the close letter moving the assignee's row — is
/// a mailspace event like any other, so it wakes that role's watcher.
#[test]
fn an_event_applied_by_pull_wakes_the_assignees_watcher() {
    let server = FakeServer::new();
    let a = checkout("alice", &["mind"]);
    let b = checkout("bob", &["mind", "hand"]);
    let (ca, cb) = (a.client(&server), b.client(&server));
    let sent = a
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![b.address("hand")],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "ship it".into(),
            body: "done_when: it works\n".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let task = sent
        .delivered
        .first()
        .map_or(sent.sent, |delivered| delivered.handle.clone());
    a.push(&ca);
    b.pull(&cb);
    assert!(
        !wakes_moved(&b.mailspace, "hand", "task"),
        "the delivery alone is not the close"
    );

    // The owner's close is a letter; only the pull applies it here.
    a.mailspace
        .move_task("mind", &task, "done", None, None, &[], &[])
        .unwrap();
    a.push(&ca);
    b.pull(&cb);
    assert!(
        wakes_moved(&b.mailspace, "hand", "task"),
        "the moved event a pull applied wakes the assignee's watcher"
    );
}
