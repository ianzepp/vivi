use std::cell::Cell;

use super::*;
use crate::chat::{ChatConfig, chat_dir};

const ROOM: &str = "abcde-fghjk-mnpqr-stvwz-23456";
const ME: &str = "beta.checkout-b";
const THEM: &str = "alpha.checkout-a";

struct Fixture {
    mailspace: Mailspace,
    state: ChatState,
    room: JoinedRoom,
    _tmp: tempfile::TempDir,
}

fn fixture(recipient: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity(recipient).unwrap();
    mailspace.config.chat = Some(ChatConfig {
        recipient: Some(recipient.into()),
        ..ChatConfig::default()
    });
    let state = ChatState::open(&chat_dir(&mailspace.dir)).unwrap();
    state.join_room(ROOM, 2, ME).unwrap();
    let room = state.room(ROOM).unwrap().unwrap();
    Fixture {
        mailspace,
        state,
        room,
        _tmp: tmp,
    }
}

/// A traveling id whose 8-character handle is `n` in hex, so fixtures with
/// different numbers never share a handle (real ids are hash-spread).
fn traveling(n: u64) -> String {
    format!("msg_{n:08x}{:016x}", 0)
}

fn item(n: u64, kind: Option<&str>, from: &str, to: Option<&str>) -> ChatMessage {
    ChatMessage {
        message_id: traveling(n),
        cursor: n,
        from: from.into(),
        fingerprint: "0123456789abcdef".into(),
        to: to.map(String::from),
        subject: format!("subject {n}"),
        body: format!("body {n}\n"),
        created_at: "2026-10-09T18:00:00Z".into(),
        kind: kind.map(String::from),
        in_reply_to: None,
        refs: Vec::new(),
    }
}

fn ingest(fx: &Fixture, recipient: &str, messages: &[ChatMessage]) -> PullReport {
    ingest_page(&fx.mailspace, &fx.room, recipient, messages).unwrap()
}

fn page(messages: Vec<ChatMessage>, next_cursor: u64) -> PollPage {
    PollPage {
        messages,
        next_cursor,
    }
}

#[test]
fn items_file_by_kind_under_their_traveling_ids() {
    let fx = fixture("mind");
    let messages = [
        item(1, Some("need"), THEM, Some(ME)),
        item(2, Some("task"), THEM, None),
        item(3, Some("want"), THEM, Some(ME)),
        item(4, None, THEM, None),
    ];
    let report = ingest(&fx, "mind", &messages);

    assert_eq!(report.fetched, 4);
    assert_eq!(report.filed.get("needs"), Some(&1));
    assert_eq!(report.filed.get("tasks"), Some(&1));
    assert_eq!(report.filed.get("wants"), Some(&1));
    assert_eq!(report.filed.get("inbox"), Some(&1));

    let storage = fx.mailspace.storage().unwrap();
    for (role, n) in [("needs", 1), ("tasks", 2), ("wants", 3), ("inbox", 4)] {
        let held = storage.list_messages_by_role(role).unwrap();
        assert_eq!(held.len(), 1, "{role}");
        assert_eq!(held[0].message_id, traveling(n), "{role} keeps the wire id");
        assert_eq!(held[0].account, "mind");
        assert!(!held[0].read_state);
        let binding = held[0].remote.as_ref().expect("one room binding");
        assert_eq!(binding.provider, "vivi-room");
        assert_eq!(binding.remote_mailbox, ROOM);
        assert_eq!(u64::from(binding.remote_uid), n);
    }
    // D2: the handle is the 8-character prefix of the traveling id.
    let need = &storage.list_messages_by_role("needs").unwrap()[0];
    assert_eq!(need.handle, "00000001");
}

#[test]
fn new_work_items_mint_backlog_nodes_but_plain_mail_does_not() {
    let fx = fixture("mind");
    ingest(
        &fx,
        "mind",
        &[
            item(0xaaaa_0001, Some("need"), THEM, Some(ME)),
            item(0x1111_0002, None, THEM, None),
        ],
    );
    let storage = fx.mailspace.storage().unwrap();
    let need = storage.display_handle(&traveling(0xaaaa_0001)).unwrap();
    let mail = storage.display_handle(&traveling(0x1111_0002)).unwrap();

    assert!(fx.mailspace.backlog_item_tracked(&need).unwrap());
    assert!(!fx.mailspace.backlog_item_tracked(&mail).unwrap());
}

#[test]
fn delivery_events_are_recorded_for_watchers() {
    let fx = fixture("mind");
    ingest(&fx, "mind", &[item(1, Some("need"), THEM, Some(ME))]);
    let storage = fx.mailspace.storage().unwrap();
    let events = storage.list_mailspace_events(&traveling(1)).unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "chat pull");
    assert_eq!(events[0].event_type, "delivered");
    assert_eq!(events[0].to_role.as_deref(), Some("needs"));
    assert_eq!(events[0].to_identity.as_deref(), Some("mind"));
}

#[test]
fn replay_is_idempotent_and_never_resets_local_state() {
    let fx = fixture("mind");
    let messages = [
        item(1, Some("need"), THEM, Some(ME)),
        item(2, None, THEM, None),
    ];
    ingest(&fx, "mind", &messages);
    {
        let mut storage = fx.mailspace.storage().unwrap();
        storage
            .move_message_to_role("mind", &traveling(1), "done")
            .unwrap();
        storage
            .set_local_read_state("mind", &traveling(2), true)
            .unwrap();
    }

    let replay = ingest(&fx, "mind", &messages);

    assert_eq!(replay.existing, 2);
    assert!(replay.filed.is_empty());
    let storage = fx.mailspace.storage().unwrap();
    assert_eq!(
        storage
            .message_by_id(&traveling(1))
            .unwrap()
            .unwrap()
            .local_role,
        "done",
        "a closed need stays closed"
    );
    assert!(
        storage
            .message_by_id(&traveling(2))
            .unwrap()
            .unwrap()
            .read_state
    );
    assert_eq!(storage.list_messages().unwrap().len(), 2);
    assert_eq!(
        storage.list_mailspace_events(&traveling(1)).unwrap().len(),
        1
    );
}

#[test]
fn direct_items_for_other_seats_are_counted_not_filed() {
    let fx = fixture("mind");
    let report = ingest(
        &fx,
        "mind",
        &[
            item(1, Some("need"), THEM, Some("gamma.checkout-c")),
            item(2, Some("need"), THEM, Some(ME)),
        ],
    );

    assert_eq!(report.not_addressed, 1);
    assert_eq!(
        fx.mailspace
            .storage()
            .unwrap()
            .list_messages()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn own_items_file_as_read_sent_copies_without_backlog_nodes() {
    let fx = fixture("mind");
    ingest(&fx, "mind", &[item(5, Some("need"), ME, Some(THEM))]);
    let storage = fx.mailspace.storage().unwrap();
    let held = storage.list_messages_by_role("sent").unwrap();

    assert_eq!(held.len(), 1);
    assert!(held[0].read_state);
    assert_eq!(held[0].message_id, traveling(5));
    assert!(storage.list_messages_by_role("needs").unwrap().is_empty());
    let handle = storage.display_handle(&traveling(5)).unwrap();
    assert!(!fx.mailspace.backlog_item_tracked(&handle).unwrap());
    let events = storage.list_mailspace_events(&traveling(5)).unwrap();
    assert_eq!(events[0].event_type, "sent_copy_created");
}

#[test]
fn legacy_wire_ids_file_under_stable_store_local_ids() {
    let fx = fixture("mind");
    let mut legacy = item(1, None, THEM, None);
    legacy.message_id = "9f8e7d6c5b4a39281706f5e4d3c2b1a0@0123abcd".into();
    let first = ingest(&fx, "mind", std::slice::from_ref(&legacy));
    let second = ingest(&fx, "mind", std::slice::from_ref(&legacy));

    assert_eq!(first.local_ids, 1);
    assert_eq!(second.existing, 1, "the seeded id is stable across pulls");
    let storage = fx.mailspace.storage().unwrap();
    let held = storage.list_messages().unwrap();
    assert_eq!(held.len(), 1);
    assert!(held[0].message_id.starts_with("msg_"));
    assert_ne!(held[0].message_id, legacy.message_id);
}

#[test]
fn unknown_kinds_and_malformed_items_do_not_fail_the_pull() {
    let fx = fixture("mind");
    let mut odd = item(1, Some("proposal"), THEM, None);
    odd.subject = "from the future".into();
    let mut bad = item(2, Some("need"), THEM, None);
    bad.created_at = "not a date".into();
    let report = ingest(&fx, "mind", &[odd, bad, item(3, None, THEM, None)]);

    assert_eq!(report.malformed, 1);
    assert_eq!(report.filed.get("inbox"), Some(&2), "unknown kind is mail");
    assert!(report.summary().contains("1 malformed (skipped)"));
}

#[test]
fn replies_link_to_a_present_parent_and_dangle_honestly_otherwise() {
    let fx = fixture("mind");
    let mut receipt = item(2, None, THEM, None);
    receipt.in_reply_to = Some(traveling(1));
    let mut orphan = item(3, None, THEM, None);
    orphan.in_reply_to = Some(traveling(0xdead));

    let report = ingest(
        &fx,
        "mind",
        &[item(1, Some("need"), ME, Some(THEM)), receipt, orphan],
    );

    assert_eq!(report.linked, 1);
    let storage = fx.mailspace.storage().unwrap();
    let parent = storage.message_by_id(&traveling(1)).unwrap().unwrap();
    let child = storage.message_by_id(&traveling(2)).unwrap().unwrap();
    let link = storage.mailspace_link_for_child(&child.content_id).unwrap();
    assert_eq!(link.unwrap().parent_content_id, parent.content_id);
    let dangling = storage.message_by_id(&traveling(3)).unwrap().unwrap();
    assert!(
        storage
            .mailspace_link_for_child(&dangling.content_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn two_mailspaces_agree_on_id_handle_and_content() {
    let a = fixture("mind");
    let b = fixture("hand");
    let messages = [item(0xabcdef, Some("need"), THEM, None)];
    ingest(&a, "mind", &messages);
    ingest(&b, "hand", &messages);

    let held_a = a.mailspace.storage().unwrap().list_messages().unwrap();
    let held_b = b.mailspace.storage().unwrap().list_messages().unwrap();
    assert_eq!(held_a[0].message_id, held_b[0].message_id);
    assert_eq!(held_a[0].handle, held_b[0].handle);
    assert_eq!(
        held_a[0].content_id, held_b[0].content_id,
        "the bytes carry no local identity"
    );
    assert_ne!(held_a[0].account, held_b[0].account);
}

#[test]
fn the_sync_cursor_advances_only_after_a_successful_page() {
    let fx = fixture("mind");
    let since = Cell::new(u64::MAX);
    let run = |from_zero: bool, reply: Result<PollPage, VivariumError>| {
        pull_with(&fx.mailspace, &fx.state, &fx.room, "mind", from_zero, |s| {
            since.set(s);
            reply
        })
    };

    assert!(run(false, Err(VivariumError::Message("down".into()))).is_err());
    assert_eq!(fx.state.sync_cursor(ROOM).unwrap(), None);

    let report = run(
        false,
        Ok(page(
            vec![item(1, None, THEM, None), item(2, None, THEM, None)],
            2,
        )),
    )
    .unwrap();
    assert_eq!((since.get(), report.next_cursor), (0, 2));
    assert_eq!(fx.state.sync_cursor(ROOM).unwrap().as_deref(), Some("2"));

    run(false, Ok(page(Vec::new(), 2))).unwrap();
    assert_eq!(since.get(), 2, "the next pull resumes at the stored cursor");

    let resync = run(
        true,
        Ok(page(
            vec![item(1, None, THEM, None), item(2, None, THEM, None)],
            2,
        )),
    )
    .unwrap();
    assert_eq!(since.get(), 0, "resync replays from cursor zero");
    assert_eq!(resync.existing, 2);
}

#[test]
fn the_sync_cursor_is_independent_of_the_read_cursor() {
    let fx = fixture("mind");
    fx.state.set_cursor(ROOM, "40").unwrap();
    let since = Cell::new(u64::MAX);
    pull_with(&fx.mailspace, &fx.state, &fx.room, "mind", false, |s| {
        since.set(s);
        Ok(page(Vec::new(), s))
    })
    .unwrap();

    assert_eq!(
        since.get(),
        0,
        "reading a room must not skip unsynced items"
    );
    assert_eq!(fx.state.cursor(ROOM).unwrap().as_deref(), Some("40"));
}

#[test]
fn recipient_is_required_explicit_and_must_be_a_known_role() {
    let fx = fixture("mind");
    assert_eq!(resolve_recipient(&fx.mailspace).unwrap(), "mind");

    let mut unset = fx.mailspace.clone();
    unset.config.chat = Some(ChatConfig::default());
    let error = resolve_recipient(&unset).unwrap_err().to_string();
    assert!(error.contains("[chat] recipient"), "{error}");

    let mut unknown = fx.mailspace.clone();
    unknown.config.chat = Some(ChatConfig {
        recipient: Some("nobody".into()),
        ..ChatConfig::default()
    });
    assert!(
        resolve_recipient(&unknown)
            .unwrap_err()
            .to_string()
            .contains("unknown local role")
    );
}

#[test]
fn leaving_a_room_forgets_its_sync_cursor() {
    let fx = fixture("mind");
    fx.state.set_sync_cursor(ROOM, "9").unwrap();
    fx.state.leave_room(ROOM).unwrap();

    assert_eq!(fx.state.sync_cursor(ROOM).unwrap(), None);
}

#[test]
fn a_colliding_id_is_refused_without_wedging_the_rest_of_the_pull() {
    let fx = fixture("mind");
    ingest(&fx, "mind", &[item(1, Some("need"), THEM, Some(ME))]);
    let mut hostile = item(1, Some("need"), "mallory", Some(ME));
    hostile.body = "swapped in".into();

    let report = ingest(
        &fx,
        "mind",
        &[
            hostile,
            item(2, None, THEM, None),
            item(3, Some("task"), THEM, None),
        ],
    );

    assert_eq!(report.conflicts, vec![traveling(1)]);
    assert_eq!(
        report.filed.get("inbox"),
        Some(&1),
        "later items still file"
    );
    assert_eq!(report.filed.get("tasks"), Some(&1));
    assert!(report.summary().contains("1 refused"));
    let storage = fx.mailspace.storage().unwrap();
    let held = storage.read_message(&traveling(1)).unwrap();
    assert!(
        !String::from_utf8_lossy(&held).contains("swapped in"),
        "the original bytes stand"
    );
}
