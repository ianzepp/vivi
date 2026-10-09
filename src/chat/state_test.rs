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
