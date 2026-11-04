//! The `vivi sync status` epoch line: the pure line the status command
//! prints for the epoch its sync state records.

use crate::sync::state::SyncState;

#[test]
fn the_status_line_names_a_stored_epoch() {
    let state = SyncState {
        epoch: Some(4294967295),
        ..SyncState::default()
    };

    assert_eq!(state.epoch_line(), "epoch: 4294967295");
}

#[test]
fn the_status_line_says_when_the_server_sends_no_epoch() {
    let state = SyncState::default();

    assert_eq!(state.epoch_line(), "epoch: not sent by the server");
}

#[test]
fn a_missing_state_file_reads_as_no_epoch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");

    let state = SyncState::load(&dir, "prj").unwrap();

    assert_eq!(state.epoch, None);
    assert_eq!(state.epoch_line(), "epoch: not sent by the server");
}
