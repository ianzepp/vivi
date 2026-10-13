//! The epoch as the validity of remote bindings. A wiped and restored
//! server reuses sequence numbers, so `remote_uid = sequence` is only
//! unique together with the epoch the log was read under: letters filed by
//! pull and letters bound by push carry the epoch as their
//! `remote_uidvalidity`, and the fixed [`UIDVALIDITY`] only while the
//! server sends none.

use super::*;

use crate::sync::UIDVALIDITY;
use crate::sync::fake_server::{PROJECT, World, wire_id};
use crate::sync::state::SyncState;
use crate::sync::sync_dir;

#[test]
fn letters_pulled_at_the_first_sight_of_an_epoch_are_bound_under_it() {
    let world = World::new();
    world.put(1, "first", "b1");
    world.server.set_epoch(Some(7));

    world.bob.pull(&world.bob_client);

    let row = world.bob.row(&wire_id(1, 0)).expect("the letter is held");
    let remote = row.remote.expect("the letter is bound to the project");
    assert_eq!(remote.remote_uidvalidity, 7, "the epoch is the validity");
    assert_eq!(remote.remote_uid, 1, "the sequence");
}

#[test]
fn a_push_after_the_epoch_is_known_binds_letters_under_it() {
    let world = World::new();
    let handle = world
        .alice
        .send_need("mind", &world.bob.address("mind"), "to bob");
    let dir = sync_dir(&world.alice.mailspace.dir);
    let mut state = SyncState::load(&dir, PROJECT).unwrap();
    state.epoch = Some(42);
    state.save(&dir).unwrap();

    let report = world.alice.push(&world.alice.client(&world.server));

    assert_eq!(report.sent, 1, "{:?}", report.lines());
    let row = world
        .alice
        .row(&world.alice.id_of(&handle))
        .expect("the sent letter is held");
    let remote = row.remote.expect("the pushed letter is bound");
    assert_eq!(remote.remote_uidvalidity, 42, "the epoch is the validity");
}

#[test]
fn a_push_without_an_epoch_still_binds_under_the_fixed_validity() {
    let world = World::new();
    let handle = world
        .alice
        .send_need("mind", &world.bob.address("mind"), "to bob");

    let report = world.alice.push(&world.alice.client(&world.server));

    assert_eq!(report.sent, 1, "{:?}", report.lines());
    let row = world
        .alice
        .row(&world.alice.id_of(&handle))
        .expect("the sent letter is held");
    let remote = row.remote.expect("the pushed letter is bound");
    assert_eq!(remote.remote_uidvalidity, UIDVALIDITY);
}
