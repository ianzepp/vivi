use super::*;

#[test]
fn rooms_cursors_and_outbox_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ChatState::open(tmp.path()).expect("open chat state");
    assert_eq!(state.joined_room_count().expect("count"), 0);

    state.join_room("abcde-1", 2, "burgus/vivi").expect("join");
    state.join_room("abcde-1", 3, "other/vivi").expect("rebind");
    assert_eq!(state.joined_room_count().expect("count"), 1);

    assert_eq!(state.cursor("abcde-1").expect("cursor"), None);
    state.set_cursor("abcde-1", "msg-9").expect("set cursor");
    assert_eq!(
        state.cursor("abcde-1").expect("cursor"),
        Some("msg-9".into())
    );

    let id = state
        .enqueue("abcde-1", "msg-10", "{\"body\":\"hi\"}")
        .expect("enqueue");
    assert_eq!(state.outbox_depth().expect("depth"), 1);
    state.mark_flushed(id).expect("flush");
    assert_eq!(state.outbox_depth().expect("depth"), 0);
}

#[test]
fn rooms_listing_and_lookup() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ChatState::open(tmp.path()).expect("open chat state");
    state.join_room("room-a", 1, "a/x").expect("join a");
    state.join_room("room-b", 3, "b/y").expect("join b");
    let rooms = state.rooms().expect("rooms");
    assert_eq!(rooms.len(), 2);
    assert_eq!(rooms[0].room_id, "room-a");
    assert_eq!(rooms[0].seat, 1);
    assert_eq!(rooms[0].label, "a/x");
    assert!(!rooms[1].joined_at.is_empty());
    let found = state.room("room-b").expect("lookup").expect("present");
    assert_eq!((found.seat, found.label.as_str()), (3, "b/y"));
    assert!(state.room("room-z").expect("lookup").is_none());
}

#[test]
fn state_reopen_keeps_rows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    {
        let state = ChatState::open(tmp.path()).expect("open chat state");
        state.join_room("room-x", 1, "a/b").expect("join");
    }
    let state = ChatState::open(tmp.path()).expect("reopen chat state");
    assert_eq!(state.joined_room_count().expect("count"), 1);
    assert!(tmp.path().join(STATE_FILE).exists());
    assert!(tmp.path().join(".gitignore").exists());
}

#[test]
fn leave_room_removes_row_and_cursor() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ChatState::open(tmp.path()).expect("open chat state");
    state.join_room("room-a", 1, "a/x").expect("join");
    state.set_cursor("room-a", "9").expect("cursor");

    assert!(state.leave_room("room-a").expect("leave"), "row removed");
    assert!(state.room("room-a").expect("lookup").is_none());
    assert_eq!(state.cursor("room-a").expect("cursor"), None);
    assert!(
        !state.leave_room("room-a").expect("leave again"),
        "idempotent"
    );
}

#[test]
fn a_rejected_row_leaves_the_pending_list_and_stays_listed_with_its_reason() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ChatState::open(tmp.path()).expect("state");
    let first = state.enqueue("room", "msg-1", "{}").expect("enqueue");
    state.enqueue("room", "msg-2", "{}").expect("enqueue");

    state
        .mark_rejected(first, "unknown recipient")
        .expect("mark");

    assert_eq!(state.outbox_depth().expect("depth"), 1);
    let pending: Vec<String> = state
        .pending_outbox()
        .expect("pending")
        .into_iter()
        .map(|row| row.message_id)
        .collect();
    assert_eq!(pending, ["msg-2"]);
    let rejected = state.rejected_outbox().expect("rejected");
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].message_id, "msg-1");
    assert_eq!(rejected[0].reason, "unknown recipient");
}
