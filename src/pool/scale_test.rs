use super::*;
use crate::pool::fake_provider::{claimed, pid, slot};
use crate::pool::slot::{self, META_STATE};

/// Slots 1..=n of pool `p`, in the given states.
fn pool(states: &[&str]) -> Vec<SlotStatus> {
    let machines: Vec<_> = states
        .iter()
        .enumerate()
        .map(|(i, state)| {
            let n = u32::try_from(i + 1).unwrap();
            let id = format!("m{n}");
            let name = format!("ms-p-slot-{n}");
            match *state {
                "free" => slot(&id, &name, "stopped", "p", n),
                "busy" => claimed(&id, &name, "started", "p", "task0001"),
                "finished" => claimed(&id, &name, "stopped", "p", "task0002"),
                "seeding" => {
                    let mut m = slot(&id, &name, "started", "p", n);
                    m.metadata.insert(META_STATE.into(), "seeding".into());
                    m
                }
                other => panic!("no state {other}"),
            }
        })
        .collect();
    let mut machines = machines;
    for (i, m) in machines.iter_mut().enumerate() {
        m.metadata
            .insert(slot::META_SLOT.into(), (i + 1).to_string());
    }
    slot::pool_slots(&pid("p"), &machines)
}

fn names(slots: &[SlotStatus]) -> Vec<&str> {
    slots.iter().map(|s| s.name.as_str()).collect()
}

#[test]
fn scaling_up_creates_the_difference_and_destroys_nothing() {
    let plan = plan(&pool(&["free", "free"]), 5, 8).unwrap();
    assert_eq!((plan.have, plan.create), (2, 3));
    assert!(plan.destroy.is_empty() && plan.skipped.is_empty());
    assert_eq!(plan.ends_with(), 5);
}

#[test]
fn scaling_to_the_size_it_is_does_nothing() {
    let plan = plan(&pool(&["free", "busy"]), 2, 8).unwrap();
    assert_eq!(plan.create, 0);
    assert!(plan.destroy.is_empty() && plan.skipped.is_empty());
}

#[test]
fn scaling_down_destroys_free_slots_highest_index_first() {
    let plan = plan(&pool(&["free", "free", "free", "free"]), 1, 8).unwrap();
    assert_eq!(
        names(&plan.destroy),
        ["ms-p-slot-4", "ms-p-slot-3", "ms-p-slot-2"]
    );
    assert!(plan.skipped.is_empty());
    assert_eq!(plan.ends_with(), 1);
}

#[test]
fn a_busy_seeding_or_uncollected_slot_is_skipped_and_a_lower_free_one_goes() {
    let plan = plan(
        &pool(&["free", "free", "busy", "seeding", "finished"]),
        3,
        8,
    )
    .unwrap();
    // Two must go. Slot 5 (finished-uncollected) and 4 (seeding) are skipped;
    // the highest free ones are 2 and 1.
    assert_eq!(names(&plan.destroy), ["ms-p-slot-2", "ms-p-slot-1"]);
    assert_eq!(
        names(&plan.skipped),
        ["ms-p-slot-5", "ms-p-slot-4", "ms-p-slot-3"]
    );
    assert_eq!(plan.skipped[0].state, "finished-uncollected");
    assert_eq!(plan.skipped[1].state, "seeding");
    assert_eq!(plan.skipped[2].state, "busy");
    assert_eq!(plan.ends_with(), 3);
}

#[test]
fn it_stops_destroying_once_the_target_is_reached() {
    let plan = plan(&pool(&["free", "busy", "free"]), 2, 8).unwrap();
    assert_eq!(names(&plan.destroy), ["ms-p-slot-3"]);
    assert!(plan.skipped.is_empty(), "nothing was passed over");
}

#[test]
fn scale_to_zero_empties_the_pool_of_free_slots_only() {
    let plan = plan(&pool(&["free", "busy", "free", "finished"]), 0, 8).unwrap();
    assert_eq!(names(&plan.destroy), ["ms-p-slot-3", "ms-p-slot-1"]);
    assert_eq!(names(&plan.skipped), ["ms-p-slot-4", "ms-p-slot-2"]);
    assert_eq!(plan.ends_with(), 2, "the busy and uncollected slots stay");
}

#[test]
fn above_the_configured_maximum_is_refused() {
    let said = plan(&pool(&["free"]), 9, 8).unwrap_err().to_string();
    assert!(said.contains("at most 8"), "{said}");
    assert!(plan(&pool(&["free"]), 8, 8).is_ok());
}
