//! Admission end to end on the fake server: the whole linking route run
//! with only the vivi verbs — create, refuse, vouch, join, sync.

use chrono::Utc;

use crate::sync::SyncSettings;
use crate::sync::fake_server::{Checkout, FakeServer, checkout};
use crate::sync::identity::SyncIdentity;
use crate::sync::vouch::{compose_vouch, send_vouch};

/// Point an existing checkout's `[sync]` at `project` and re-resolve.
fn join_project(checkout: &mut Checkout, project: &str) {
    checkout.mailspace.config.sync.as_mut().unwrap().project = Some(project.to_string());
    checkout.settings = SyncSettings::resolve(&checkout.mailspace).unwrap();
}

#[test]
fn a_second_mailspace_is_admitted_using_only_the_vivi_verbs() {
    let server = FakeServer::new();
    let mut alice = checkout("alice", &["mind"]);
    let mut bob = checkout("bob", &["mind"]);
    let alice_pem = SyncIdentity::ensure(&alice.mailspace.dir)
        .unwrap()
        .public_key_pem();
    let bob_pem = SyncIdentity::ensure(&bob.mailspace.dir)
        .unwrap()
        .public_key_pem();

    // A creates the project through the fake; the creator is admitted.
    let project = server.create_project(&alice_pem, "alice");
    join_project(&mut alice, &project);
    join_project(&mut bob, &project);

    // B's join is refused while no vouch for the name exists.
    let refused = bob.client(&server).join("bob", &bob_pem).unwrap_err();
    assert!(refused.contains("admission_required"), "{refused}");

    // A composes and sends the vouch with the real verbs, on A's client.
    let letter = compose_vouch(
        &alice.settings.name,
        &alice.mailspace.address_domain(),
        "bob",
        &bob_pem,
        Utc::now(),
    )
    .unwrap();
    send_vouch(&alice.client(&server), &letter).unwrap();

    // B joins, pushes a letter, and A pulls it: the member really syncs.
    bob.client(&server).join("bob", &bob_pem).unwrap();
    let sent = bob.send_need("mind", &alice.address("mind"), "hello from bob");
    bob.push(&bob.client(&server));
    alice.pull(&alice.client(&server));
    let id = bob.id_of(&sent);
    assert!(alice.row(&id).is_some(), "the pushed letter is pulled");
}

#[test]
fn a_vouch_for_a_name_and_key_does_not_admit_that_name_with_another_key() {
    let server = FakeServer::new();
    let mut alice = checkout("alice", &["mind"]);
    let bob = checkout("bob", &[]);
    let alice_pem = SyncIdentity::ensure(&alice.mailspace.dir)
        .unwrap()
        .public_key_pem();
    let bob_pem = SyncIdentity::ensure(&bob.mailspace.dir)
        .unwrap()
        .public_key_pem();
    // Another key for the same name: the right PEM shape, other bytes.
    let other_pem = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAzEAgLVOmuQxwQH1SCgd/nsjXBTAKPTNMOiCOUVJ1tBMnNj1tw==\n-----END PUBLIC KEY-----";

    let project = server.create_project(&alice_pem, "alice");
    join_project(&mut alice, &project);

    let letter = compose_vouch(
        &alice.settings.name,
        &alice.mailspace.address_domain(),
        "bob",
        &bob_pem,
        Utc::now(),
    )
    .unwrap();
    send_vouch(&alice.client(&server), &letter).unwrap();

    // The vouched key joins; the same name under another key is refused.
    bob.client(&server).join("bob", &bob_pem).unwrap();
    let refused = bob.client(&server).join("bob", other_pem).unwrap_err();
    assert!(refused.contains("admission_required"), "{refused}");
}
