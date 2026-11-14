use super::*;
use crate::data::config::{DeclaredPool, DeclaredSlot};

fn known(pool: &str, slot: &str) -> KnownSlot {
    KnownSlot {
        pool: pool.into(),
        slot: slot.into(),
        machine: "m1".into(),
        last_dispatched_at: 100,
        last_outcome: Some("finished".into()),
        last_collected_at: Some(200),
    }
}

fn open_run(pool: &str, slot: &str, at: i64, job: &str) -> RunRow {
    RunRow {
        job: job.into(),
        task_message_id: "msg_0123456789abcdef".into(),
        need: None,
        pool: pool.into(),
        slot: slot.into(),
        machine: "m2".into(),
        model: "x".into(),
        kind: None,
        dispatched_at: at,
        budget_s: None,
        collected_at: None,
        outcome: None,
        agent_rc: None,
        check_rc: None,
        job_s: None,
        landing: None,
    }
}

#[test]
fn slot_names_sort_by_their_number() {
    let slots = merge(
        vec![
            known("p", "slot-10"),
            known("p", "slot-2"),
            known("p", "slot-1"),
        ],
        Vec::new(),
    );
    let names: Vec<&str> = slots.iter().map(|slot| slot.slot.as_str()).collect();
    assert_eq!(names, ["slot-1", "slot-2", "slot-10"]);
}

#[test]
fn an_open_run_makes_a_slot_busy_and_the_newest_wins() {
    let slots = merge(
        vec![known("p", "slot-1"), known("p", "slot-2")],
        vec![
            open_run("p", "slot-1", 500, "newer"),
            open_run("p", "slot-1", 300, "older"),
        ],
    );
    assert_eq!(slots.len(), 2);
    match &slots[0].state {
        SlotState::Busy(run) => assert_eq!(run.job, "newer"),
        other => panic!("expected busy, got {other:?}"),
    }
    assert_eq!(slots[0].machine, "m2");
    assert!(matches!(slots[1].state, SlotState::Free(Some(_))));
}

#[test]
fn an_open_run_on_a_slot_with_no_recent_history_still_lists() {
    let slots = merge(Vec::new(), vec![open_run("p", "slot-9", 1, "j")]);
    assert_eq!(slots.len(), 1);
}

#[test]
fn declared_slots_that_never_ran_are_counted_not_named() {
    let declared = vec![
        DeclaredPool {
            name: "p".into(),
            slots: 4,
        },
        DeclaredPool {
            name: "q".into(),
            slots: 1,
        },
    ];
    let slots = merge(vec![known("p", "slot-1"), known("q", "slot-1")], Vec::new());
    let unseen = unseen_slots(&declared, &slots);
    assert_eq!(
        unseen,
        vec![UnseenSlots {
            pool: "p".into(),
            count: 3
        }]
    );
}

fn named(name: &str, host: &str, pools: &[&str]) -> DeclaredSlot {
    DeclaredSlot {
        name: name.into(),
        host: host.into(),
        pools: pools.iter().map(ToString::to_string).collect(),
    }
}

#[test]
fn named_slots_that_never_ran_are_listed_free_in_each_pool_they_serve() {
    let slots = add_declared(
        merge(vec![known("p", "slot-1")], Vec::new()),
        &[
            named("radix-2", "studio-01", &["p", "q"]),
            named("slot-1", "studio-09", &["p"]),
            named("radix-10", "studio-01", &["p"]),
        ],
    );
    let listed: Vec<(&str, &str)> = slots
        .iter()
        .map(|slot| (slot.pool.as_str(), slot.slot.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("p", "radix-2"),
            ("p", "radix-10"),
            ("p", "slot-1"),
            ("q", "radix-2")
        ]
    );
    let never_ran = &slots[0];
    assert_eq!(never_ran.machine, "studio-01");
    assert_eq!(never_ran.state, SlotState::Free(None));
    let ran = &slots[2];
    assert_eq!(ran.machine, "m1", "a slot with a record keeps its record");
    assert!(matches!(ran.state, SlotState::Free(Some(_))));
}

#[test]
fn named_slots_count_against_a_pools_cap_so_only_the_rest_are_unseen() {
    let declared = vec![DeclaredPool {
        name: "p".into(),
        slots: 4,
    }];
    let slots = add_declared(
        Vec::new(),
        &[named("a", "h", &["p"]), named("b", "h", &["p"])],
    );
    let unseen = unseen_slots(&declared, &slots);
    assert_eq!(
        unseen,
        vec![UnseenSlots {
            pool: "p".into(),
            count: 2
        }]
    );
}
