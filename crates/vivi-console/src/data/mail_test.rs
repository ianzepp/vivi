use vivi_storage::storage::{MessageIngestRequest, Storage, StoredMessage};

use super::*;
use crate::test_support::{Seeded, seeded};

const ROOT_ID: &str = "msg_aaaa1111bbbb2222cccc3333dddd4444";
const ROOT_SENT_ID: &str = "msg_1234abcdffff0000cccc3333dddd4444";
const REPLY_ID: &str = "msg_eeee5555ffff6666aaaa7777bbbb8888";
const OTHER_ID: &str = "msg_9999000011112222333344445555aaaa";

pub(crate) const CONFIG: &str = "name = \"demo\"\n\n\
    [[identities]]\nname = \"mind\"\n\n\
    [[identities]]\nname = \"alice\"\naliases = [\"ally\"]\n";

fn eml(subject: &str, date: &str, kind: Option<&str>, body: &str) -> String {
    let kind = kind.map_or_else(String::new, |kind| format!("X-Vivi-Kind: {kind}\r\n"));
    format!(
        "From: alice@x.test\r\nTo: mind@x.test\r\nSubject: {subject}\r\n\
         Date: {date}\r\nMessage-ID: <{subject}@x.test>\r\n{kind}\r\n{body}\r\n"
    )
}

fn put(storage: &Storage, account: &str, role: &str, id: &str, eml: &str) -> StoredMessage {
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: Some(id.into()),
                presented_id: None,
                seed_hint: id.into(),
                remote: None,
            },
            eml.as_bytes(),
        )
        .unwrap()
}

/// A project with identities, one need (held by `mind` and, as sent, by
/// `alice`), one reply to it, and one later unrelated mail.
pub(crate) fn seed_mail() -> (Seeded, Storage) {
    let store = seeded(true);
    std::fs::write(store.root.join(".vivi/mailspace.toml"), CONFIG).unwrap();
    let storage = Storage::open_mailspace(&store.root.join(".vivi")).unwrap();
    let root = eml(
        "Root",
        "Mon, 01 Jan 2024 10:00:00 +0000",
        Some("need"),
        "Please fix <b>this</b> & that.\r\n\r\n- [ ] one",
    );
    let held = put(&storage, "mind", "needs", ROOT_ID, &root);
    put(&storage, "alice", "sent", ROOT_SENT_ID, &root);
    let reply = eml(
        "Re: Root",
        "Mon, 01 Jan 2024 11:00:00 +0000",
        None,
        "Done, see commit.",
    );
    let answer = put(&storage, "alice", "inbox", REPLY_ID, &reply);
    storage
        .link_mailspace_content(&answer.content_id, &held.content_id, "captured")
        .unwrap();
    let other = eml("Other", "Mon, 01 Jan 2024 12:00:00 +0000", None, "Hello.");
    put(&storage, "mind", "inbox", OTHER_ID, &other);
    (store, storage)
}

fn subjects(list: &MailList) -> Vec<&str> {
    list.rows.iter().map(|row| row.subject.as_str()).collect()
}

fn list_for(store: &Seeded, filter: &MailFilter) -> MailList {
    list(&store.root, filter).unwrap().unwrap()
}

#[test]
fn the_list_shows_thread_roots_newest_first_once_each() {
    let (store, _storage) = seed_mail();
    let list = list_for(&store, &MailFilter::default());
    // The need is held twice (mind/needs, alice/sent) but is one thread; the
    // reply is not a root.
    assert_eq!(subjects(&list), ["Other", "Root"]);
    let root = &list.rows[1];
    assert_eq!(root.replies, 1);
    assert!(["aaaa1111", "1234abcd"].contains(&root.handle.as_str()));
    assert_eq!(list.identities, ["alice", "ally", "mind"]);
}

#[test]
fn filters_narrow_by_identity_folder_and_kind() {
    let (store, _storage) = seed_mail();
    let by = |identity: &str, role: &str, kind: &str| {
        let filter = MailFilter {
            identity: identity.into(),
            role: role.into(),
            kind: kind.into(),
        };
        let list = list_for(&store, &filter);
        list.rows
            .iter()
            .map(|row| format!("{}/{}/{}", row.identity, row.role, row.subject))
            .collect::<Vec<_>>()
    };
    assert_eq!(by("mind", "", ""), ["mind/inbox/Other", "mind/needs/Root"]);
    assert_eq!(by("mind", "needs", ""), ["mind/needs/Root"]);
    assert_eq!(by("", "", "need"), ["mind/needs/Root"]);
    assert_eq!(by("", "", "task"), Vec::<String>::new());
    assert_eq!(by("alice", "sent", ""), ["alice/sent/Root"]);
    // The reply is filed in alice's inbox but is not a thread root.
    assert_eq!(by("alice", "inbox", ""), Vec::<String>::new());
    assert_eq!(by("", "", "mail"), ["mind/inbox/Other", "alice/sent/Root"]);
    // An unknown folder or kind matches nothing rather than everything.
    assert!(by("", "attic", "").is_empty());
    assert!(by("", "", "gizmo").is_empty());
}

#[test]
fn a_thread_reads_every_letter_in_order_from_any_letter_of_it() {
    let (store, _storage) = seed_mail();
    for token in ["aaaa1111", ROOT_ID, "eeee5555", REPLY_ID, "msg_aaaa1111"] {
        let ThreadState::Ready(thread) = thread(&store.root, token).unwrap() else {
            panic!("{token} did not resolve");
        };
        let subjects: Vec<&str> = thread.messages.iter().map(|m| m.subject.as_str()).collect();
        assert_eq!(subjects, ["Root", "Re: Root"], "{token}");
    }
    let ThreadState::Ready(thread) = thread(&store.root, "aaaa1111").unwrap() else {
        panic!();
    };
    let (root, reply) = (&thread.messages[0], &thread.messages[1]);
    assert_eq!(root.kind.as_deref(), Some("need"));
    assert_eq!(root.body, "Please fix <b>this</b> & that.\n\n- [ ] one\n");
    assert_eq!(root.parent_handle, None);
    // The handle shown is the lowest-id message holding the letter.
    assert_eq!(reply.parent_handle.as_deref(), Some("1234abcd"));
    assert_eq!(reply.body, "Done, see commit.\n");
    assert_eq!(reply.from, "alice@x.test");
    assert!(root.note.is_none());
}

#[test]
fn an_unrelated_letter_is_its_own_thread() {
    let (store, _storage) = seed_mail();
    let ThreadState::Ready(thread) = thread(&store.root, "99990000").unwrap() else {
        panic!();
    };
    assert_eq!(thread.messages.len(), 1);
}

#[test]
fn unknown_empty_and_ambiguous_handles_are_told_apart() {
    let (store, storage) = seed_mail();
    assert_eq!(thread(&store.root, "zzzz").unwrap(), ThreadState::NotFound);
    assert_eq!(thread(&store.root, "  ").unwrap(), ThreadState::NotFound);
    // A second letter sharing the eight-character handle of the first.
    let twin = eml("Twin", "Mon, 01 Jan 2024 13:00:00 +0000", None, "twin");
    put(
        &storage,
        "mind",
        "inbox",
        "msg_aaaa1111zzzz9999aaaa7777bbbb8888",
        &twin,
    );
    let ThreadState::Ambiguous(ids) = thread(&store.root, "aaaa1111").unwrap() else {
        panic!("expected ambiguity");
    };
    assert_eq!(ids.len(), 2, "{ids:?}");
    // The full id still resolves.
    assert!(matches!(
        thread(&store.root, ROOT_ID).unwrap(),
        ThreadState::Ready(_)
    ));
}

#[test]
fn a_project_with_no_store_reads_as_none() {
    let (dir, root) = crate::test_support::project_without_store();
    std::fs::write(root.join(".vivi/mailspace.toml"), CONFIG).unwrap();
    assert!(list(&root, &MailFilter::default()).unwrap().is_none());
    assert_eq!(thread(&root, "aaaa1111").unwrap(), ThreadState::NoStore);
    drop(dir);
}

#[test]
fn a_big_letter_is_cut_with_a_note() {
    let store = seeded(true);
    std::fs::write(store.root.join(".vivi/mailspace.toml"), CONFIG).unwrap();
    let storage = Storage::open_mailspace(&store.root.join(".vivi")).unwrap();
    let big = eml(
        "Big",
        "Mon, 01 Jan 2024 10:00:00 +0000",
        None,
        &"x".repeat(200_000),
    );
    put(&storage, "mind", "inbox", OTHER_ID, &big);
    let ThreadState::Ready(thread) = thread(&store.root, "99990000").unwrap() else {
        panic!();
    };
    let message = &thread.messages[0];
    assert!(message.note.as_deref().unwrap().contains("128 KiB"));
    assert!(message.body.len() < 131_200);
}

#[test]
fn a_blob_path_cannot_leave_the_mailspace() {
    let dir = std::path::Path::new("/mailspace");
    assert!(blob_path(dir, "blobs/ab/cd.eml").is_some());
    assert!(blob_path(dir, "../secret").is_none());
    assert!(blob_path(dir, "/etc/passwd").is_none());
    assert!(blob_path(dir, "blobs/../../x").is_none());
}

#[test]
fn kinds_follow_folders() {
    assert_eq!(kind_of_role("needs"), "need");
    assert_eq!(kind_of_role("inbox"), "mail");
    assert_eq!(kind_of_role("done"), "done");
}
