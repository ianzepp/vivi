use chrono::TimeZone;

use super::*;
use crate::chat::client::{ChatMessage, PollPage};
use crate::chat::state::{ChatState, JoinedRoom};
use crate::chat::sync::{ingest_page, pull_with};
use crate::chat::{ChatConfig, chat_dir};
use crate::storage::MessageIngestRequest;

const ROOM: &str = "abcde-fghjk-mnpqr-stvwz-23456";
const OTHER_ROOM: &str = "bcdef-ghjkm-npqrs-tvwxy-z2345";
const ME: &str = "beta.checkout-b";
const THEM: &str = "alpha.checkout-a";
const OLD: &str = "2026-01-01T00:00:00Z";
const RECENT: &str = "2026-10-01T00:00:00Z";

struct Fixture {
    mailspace: Mailspace,
    state: ChatState,
    room: JoinedRoom,
    _tmp: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.config.chat = Some(ChatConfig {
        recipient: Some("mind".into()),
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

/// A traveling id whose 8-character handle is `n` in hex.
fn traveling(n: u64) -> String {
    format!("msg_{n:08x}{:016x}", 0)
}

fn item(n: u64, kind: Option<&str>, created_at: &str) -> ChatMessage {
    ChatMessage {
        message_id: traveling(n),
        cursor: n,
        from: THEM.into(),
        fingerprint: "0123456789abcdef".into(),
        to: None,
        subject: format!("subject {n}"),
        body: format!("body {n}\n"),
        created_at: created_at.into(),
        kind: kind.map(String::from),
        in_reply_to: None,
        refs: Vec::new(),
    }
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 9, 0, 0, 0).unwrap()
}

fn thirty_days() -> Duration {
    parse_age("30d").unwrap()
}

fn held_ids(fx: &Fixture) -> Vec<String> {
    let mut ids: Vec<String> = fx
        .mailspace
        .storage()
        .unwrap()
        .list_messages()
        .unwrap()
        .into_iter()
        .map(|view| view.message_id)
        .collect();
    ids.sort();
    ids
}

fn ids(ns: &[u64]) -> Vec<String> {
    let mut ids: Vec<String> = ns.iter().map(|n| traveling(*n)).collect();
    ids.sort();
    ids
}

#[test]
fn ages_accept_days_and_the_shared_interval_forms() {
    assert_eq!(parse_age("30d").unwrap(), Duration::from_secs(30 * 86_400));
    assert_eq!(parse_age(" 2d ").unwrap(), Duration::from_secs(2 * 86_400));
    assert_eq!(parse_age("12h").unwrap(), Duration::from_secs(12 * 3_600));
    assert_eq!(parse_age("90m").unwrap(), Duration::from_secs(5_400));
    for bad in ["", "d", "0d", "-1d", "1w", "soon", "1.5d"] {
        assert!(parse_age(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn only_old_non_open_unstarred_room_items_are_evicted() {
    let fx = fixture();
    ingest_page(
        &fx.mailspace,
        &fx.room,
        "mind",
        &[
            item(1, None, OLD),         // old plain chat: evictable
            item(2, Some("need"), OLD), // old open need: live work
            item(3, Some("need"), OLD), // old need, then closed: evictable
            item(4, None, OLD),         // old but starred: kept
            item(5, None, RECENT),      // recent: kept
            item(6, Some("task"), OLD), // old open task: live work
            item(7, Some("want"), OLD), // old open want: live work
        ],
    )
    .unwrap();
    {
        let mut storage = fx.mailspace.storage().unwrap();
        storage
            .move_message_to_role("mind", &traveling(3), "done")
            .unwrap();
        storage
            .update_remote_flags("mind", ROOM, 1, 4, false, true)
            .unwrap();
        // Local-origin mail with an old date has no room binding: untouchable.
        let local = b"From: a@x.local\nTo: b@x.local\nSubject: local\n\
                      Date: Thu, 01 Jan 2026 00:00:00 +0000\nMessage-ID: <l@x>\n\nbody\n";
        storage
            .ingest_message(
                &MessageIngestRequest {
                    account: "mind".into(),
                    local_role: "inbox".into(),
                    read_state: false,
                    starred: false,
                    message_id_hint: None,
                    presented_id: None,
                    seed_hint: "local-origin".into(),
                    remote: None,
                },
                local,
            )
            .unwrap();
    }

    let report = prune(&fx.mailspace, None, thirty_days(), now(), false).unwrap();

    assert_eq!(report.evicted, 2);
    let mut handles: Vec<&str> = report.items.iter().map(|i| i.handle.as_str()).collect();
    handles.sort_unstable();
    assert_eq!(handles, ["00000001", "00000003"]);
    let held = held_ids(&fx);
    for kept in [2, 4, 5, 6, 7] {
        assert!(held.contains(&traveling(kept)), "{kept} must be kept");
    }
    assert_eq!(held.len(), 6, "five kept room items plus the local mail");
}

#[test]
fn a_dry_run_selects_the_same_items_and_changes_nothing() {
    let fx = fixture();
    ingest_page(
        &fx.mailspace,
        &fx.room,
        "mind",
        &[item(1, None, OLD), item(2, None, RECENT)],
    )
    .unwrap();

    let dry = prune(&fx.mailspace, None, thirty_days(), now(), true).unwrap();

    assert!(dry.dry_run);
    assert_eq!(dry.items.len(), 1);
    assert_eq!(dry.evicted, 0);
    assert_eq!(held_ids(&fx), ids(&[1, 2]));

    let real = prune(&fx.mailspace, None, thirty_days(), now(), false).unwrap();
    assert_eq!(
        real.items, dry.items,
        "a dry run previews the real selection"
    );
    assert_eq!(held_ids(&fx), ids(&[2]));
}

#[test]
fn the_cutoff_is_the_item_date_not_when_it_was_pulled() {
    let fx = fixture();
    ingest_page(
        &fx.mailspace,
        &fx.room,
        "mind",
        &[
            item(1, None, "2026-09-08T23:59:59Z"),
            item(2, None, "2026-09-09T00:00:00Z"),
            item(3, None, "2026-09-09T00:00:01Z"),
        ],
    )
    .unwrap();

    let report = prune(&fx.mailspace, None, thirty_days(), now(), false).unwrap();

    assert_eq!(report.cutoff, "2026-09-09T00:00:00+00:00");
    assert_eq!(
        held_ids(&fx),
        ids(&[2, 3]),
        "strictly older than the cutoff"
    );
}

#[test]
fn a_room_filter_leaves_other_rooms_alone_even_after_leaving() {
    let fx = fixture();
    let other = JoinedRoom {
        room_id: OTHER_ROOM.into(),
        seat: 2,
        label: ME.into(),
        joined_at: OLD.into(),
    };
    ingest_page(&fx.mailspace, &fx.room, "mind", &[item(1, None, OLD)]).unwrap();
    ingest_page(&fx.mailspace, &other, "mind", &[item(2, None, OLD)]).unwrap();
    fx.state.leave_room(ROOM).unwrap();

    let report = prune(&fx.mailspace, Some(ROOM), thirty_days(), now(), false).unwrap();

    assert_eq!(
        report.evicted, 1,
        "items outlive `chat leave` and stay prunable"
    );
    assert_eq!(held_ids(&fx), ids(&[2]));
    assert!(
        prune(
            &fx.mailspace,
            Some("not-a-room"),
            thirty_days(),
            now(),
            true
        )
        .is_err()
    );
}

#[test]
fn prune_then_resync_rebuilds_byte_identical_replicas_with_local_state() {
    let fx = fixture();
    let page = vec![
        item(1, None, OLD),
        item(2, Some("need"), OLD),
        item(3, Some("need"), OLD),
        item(4, None, RECENT),
    ];
    ingest_page(&fx.mailspace, &fx.room, "mind", &page).unwrap();
    let before = {
        let mut storage = fx.mailspace.storage().unwrap();
        storage
            .move_message_to_role("mind", &traveling(3), "done")
            .unwrap();
        storage
            .set_local_read_state("mind", &traveling(1), true)
            .unwrap();
        [1, 3]
            .map(|n| {
                let view = storage.message_by_id(&traveling(n)).unwrap().unwrap();
                let bytes = storage.read_message(&traveling(n)).unwrap();
                (view, bytes)
            })
            .to_vec()
    };
    let report = prune(&fx.mailspace, None, thirty_days(), now(), false).unwrap();
    assert_eq!(report.evicted, 2);
    assert_eq!(held_ids(&fx), ids(&[2, 4]));
    for (view, _) in &before {
        let blob = fx.mailspace.dir.join(&view.blob_relpath);
        assert!(!blob.exists(), "an evicted blob is dropped from disk");
    }

    // A plain pull never restores: the sync cursor is already past them.
    fx.state.set_sync_cursor(ROOM, "4").unwrap();
    let plain = pull_with(&fx.mailspace, &fx.state, &fx.room, "mind", false, |since| {
        Ok(PollPage {
            messages: Vec::new(),
            next_cursor: since,
        })
    })
    .unwrap();
    assert_eq!(plain.restored, 0);
    assert_eq!(held_ids(&fx), ids(&[2, 4]));

    let resync = pull_with(&fx.mailspace, &fx.state, &fx.room, "mind", true, |since| {
        assert_eq!(since, 0, "resync replays from cursor zero");
        Ok(PollPage {
            messages: page.clone(),
            next_cursor: 4,
        })
    })
    .unwrap();

    assert_eq!(resync.restored, 2);
    assert_eq!(resync.existing, 2);
    assert!(resync.filed.is_empty(), "nothing is new, only restored");
    assert_eq!(held_ids(&fx), ids(&[1, 2, 3, 4]));
    let storage = fx.mailspace.storage().unwrap();
    for (view, bytes) in &before {
        let after = storage.message_by_id(&view.message_id).unwrap().unwrap();
        assert_eq!(after.content_id, view.content_id, "same content id");
        assert_eq!(
            &storage.read_message(&view.message_id).unwrap(),
            bytes,
            "byte-identical blob"
        );
        assert_eq!(after.local_role, view.local_role, "local role survives");
        assert_eq!(
            after.read_state, view.read_state,
            "local read flag survives"
        );
    }
}

#[test]
fn pruning_twice_evicts_nothing_the_second_time() {
    let fx = fixture();
    ingest_page(&fx.mailspace, &fx.room, "mind", &[item(1, None, OLD)]).unwrap();

    assert_eq!(
        prune(&fx.mailspace, None, thirty_days(), now(), false)
            .unwrap()
            .evicted,
        1
    );
    let again = prune(&fx.mailspace, None, thirty_days(), now(), false).unwrap();

    assert_eq!(again.evicted, 0);
    assert!(again.items.is_empty());
}
