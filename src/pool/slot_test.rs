use super::*;

fn machine(
    id: &str,
    name: &str,
    state: &str,
    pool: Option<&str>,
    index: Option<u32>,
) -> MachineRecord {
    let mut metadata = BTreeMap::new();
    if let Some(pool) = pool {
        metadata.insert(META_POOL.to_string(), pool.to_string());
    }
    if let Some(index) = index {
        metadata.insert(META_SLOT.to_string(), index.to_string());
        metadata.insert(META_BASE.to_string(), "a".repeat(40));
    }
    MachineRecord {
        id: id.into(),
        name: name.into(),
        state: state.into(),
        region: "iad".into(),
        created_at: "2026-10-11T01:00:00Z".into(),
        metadata,
        exit_code: None,
    }
}

#[test]
fn names_and_metadata() {
    assert_eq!(slot_name("faber", 3), "faber-slot-3");
    let meta = slot_metadata("faber", 3, "abc");
    assert_eq!(meta[META_POOL], "faber");
    assert_eq!(meta[META_SLOT], "3");
    assert_eq!(meta[META_BASE], "abc");
}

#[test]
fn state_words() {
    assert_eq!(slot_state("stopped"), "free");
    assert_eq!(slot_state("started"), "busy");
    assert_eq!(slot_state("destroying"), "destroying");
}

#[test]
fn only_pool_members_are_slots_in_index_order() {
    let machines = vec![
        machine("m3", "faber-slot-3", "stopped", Some("faber"), Some(3)),
        machine("m0", "slot-quad-1", "stopped", None, None),
        machine("m1", "faber-slot-1", "started", Some("faber"), Some(1)),
        machine("mx", "other-slot-1", "stopped", Some("other"), Some(1)),
    ];
    let slots = pool_slots("faber", &machines);
    let names: Vec<&str> = slots.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["faber-slot-1", "faber-slot-3"]);
    assert_eq!(slots[0].state, "busy");
    assert_eq!(slots[1].state, "free");
    assert_eq!(slots[1].base.as_deref(), Some("a".repeat(40).as_str()));
}

#[test]
fn missing_indices_fill_gaps_first() {
    let machines = vec![
        machine("m1", "p-slot-1", "stopped", Some("p"), Some(1)),
        machine("m3", "p-slot-3", "stopped", Some("p"), Some(3)),
    ];
    let slots = pool_slots("p", &machines);
    assert_eq!(missing_indices(&slots, 4), [2, 4]);
    assert_eq!(missing_indices(&slots, 3), [2]);
    assert!(missing_indices(&slots, 2).is_empty());
    assert!(missing_indices(&slots, 1).is_empty());
    assert_eq!(missing_indices(&[], 3), [1, 2, 3]);
}

#[test]
fn find_slot_by_name_or_id() {
    let machines = vec![machine("m1", "p-slot-1", "stopped", Some("p"), Some(1))];
    let slots = pool_slots("p", &machines);
    assert!(find_slot(&slots, "p-slot-1").is_some());
    assert!(find_slot(&slots, "m1").is_some());
    assert!(find_slot(&slots, "slot-quad-1").is_none());
}
