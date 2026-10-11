use super::*;
use crate::pool::fake_provider::{Fake, claimed, slot};
use crate::pool::slot::{META_CLAIMED, META_STATE, META_TASK};

fn pool_of(fake: &Fake, states: &[&str]) {
    for (i, state) in states.iter().enumerate() {
        let n = u32::try_from(i + 1).unwrap();
        fake.add(slot(
            &format!("m{n}"),
            &format!("p-slot-{n}"),
            state,
            "p",
            n,
        ));
    }
}

#[test]
fn claim_takes_the_first_free_slot_and_marks_it() {
    let fake = Fake::default();
    pool_of(&fake, &["started", "stopped", "stopped"]);
    let claim = claim_slot(&fake, "p", "abcd1234", 1000).unwrap();
    assert_eq!(claim.slot.name, "p-slot-2");
    assert_eq!(fake.meta("m2", META_TASK).as_deref(), Some("abcd1234"));
    assert_eq!(fake.meta("m2", META_CLAIMED).as_deref(), Some("1000"));
    assert!(fake.meta("m3", META_TASK).is_none());
    assert!(fake.leases.borrow().contains_key("m2"));
}

#[test]
fn a_slot_with_an_uncollected_result_is_never_claimed() {
    let fake = Fake::default();
    fake.add(claimed("m1", "p-slot-1", "stopped", "p", "oldtask1"));
    pool_of_extra(&fake);
    let err = claim_slot(&fake, "p", "newtask1", 1)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no free slot"), "{err}");
    assert!(err.contains("finished-uncollected (oldtask1)"), "{err}");
    assert_eq!(fake.meta("m1", META_TASK).as_deref(), Some("oldtask1"));
}

fn pool_of_extra(fake: &Fake) {
    let mut unseeded = slot("m2", "p-slot-2", "stopped", "p", 2);
    unseeded
        .metadata
        .insert(META_STATE.into(), "seeding".into());
    fake.add(unseeded);
}

#[test]
fn an_empty_pool_says_so() {
    let err = claim_slot(&Fake::default(), "p", "t", 1)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no slots"), "{err}");
}

#[test]
fn a_held_lease_skips_to_the_next_slot() {
    let fake = Fake::default();
    pool_of(&fake, &["stopped", "stopped"]);
    fake.leases
        .borrow_mut()
        .insert("m1".into(), "someone".into());
    let claim = claim_slot(&fake, "p", "t1", 1).unwrap();
    assert_eq!(claim.slot.name, "p-slot-2");
    assert!(fake.meta("m1", META_TASK).is_none());
}

#[test]
fn successive_claims_get_different_slots() {
    let fake = Fake::default();
    pool_of(&fake, &["stopped", "stopped"]);
    let a = claim_slot(&fake, "p", "taskaaaa", 1).unwrap();
    fake.release(&a.slot.id, &a.lease).unwrap();
    let b = claim_slot(&fake, "p", "taskbbbb", 2).unwrap();
    assert_eq!(a.slot.name, "p-slot-1");
    assert_eq!(b.slot.name, "p-slot-2");
}

#[test]
fn a_slot_claimed_between_list_and_lease_is_skipped_on_the_reread() {
    let fake = Fake::default();
    pool_of(&fake, &["stopped", "stopped"]);
    // Another dispatcher marks slot 1 in the instant between our list and our
    // lease: its lease is already released, so ours succeeds, and only the
    // re-read under the lease reveals the mark.
    *fake.after_lease.borrow_mut() = Some(Box::new(|f: &Fake, id: &str| {
        if id == "m1" {
            f.set_metadata("m1", META_TASK, "rival123").unwrap();
        }
    }));
    let claim = claim_slot(&fake, "p", "mine1234", 1).unwrap();
    assert_eq!(claim.slot.name, "p-slot-2");
    assert_eq!(fake.meta("m1", META_TASK).as_deref(), Some("rival123"));
    assert!(
        !fake.leases.borrow().contains_key("m1"),
        "lease on m1 was given back"
    );
}

#[test]
fn unclaim_drops_marks_and_lease() {
    let fake = Fake::default();
    pool_of(&fake, &["stopped"]);
    let claim = claim_slot(&fake, "p", "t", 1).unwrap();
    unclaim(&fake, &claim);
    assert!(fake.meta("m1", META_TASK).is_none());
    assert!(fake.meta("m1", META_CLAIMED).is_none());
    assert!(fake.leases.borrow().is_empty());
}
