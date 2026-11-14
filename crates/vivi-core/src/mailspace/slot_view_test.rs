use super::*;
use crate::mailspace::slot_runs::tests::{collect, dispatch, space_with_task};
use crate::pool::explicit::SlotConfig;

fn declared(name: &str) -> SlotConfig {
    SlotConfig {
        name: name.into(),
        repository: "acme/app".into(),
        host: "studio".into(),
        dir: format!("/work/{name}"),
        pools: Vec::new(),
    }
}

#[test]
fn a_declared_slot_is_free_until_a_dispatch_and_busy_until_its_collect() {
    let (_tmp, mut space, handle) = space_with_task();
    space.config.slots = vec![declared("slot-a"), declared("slot-b")];

    let rows = space.slot_rows().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.state == SlotState::Free && r.declared)
    );

    dispatch(&space, &handle, "j1", "slot-a");
    let rows = space.slot_rows().unwrap();
    assert_eq!(rows[0].state, SlotState::Busy);
    assert_eq!(rows[0].run.as_ref().unwrap().run.job, "j1");
    assert_eq!(rows[0].run.as_ref().unwrap().task, handle);
    assert_eq!(rows[1].state, SlotState::Free);

    collect(&space, &handle, "j1", "slot-a");
    assert_eq!(space.slot_rows().unwrap()[0].state, SlotState::Free);
}

#[test]
fn a_job_in_an_undeclared_slot_is_still_shown_busy() {
    let (_tmp, mut space, handle) = space_with_task();
    space.config.slots = vec![declared("slot-a")];
    dispatch(&space, &handle, "j1", "slot-x");

    let rows = space.slot_rows().unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].slot, "slot-x");
    assert!(!rows[1].declared);
    assert_eq!(rows[1].state, SlotState::Busy);
    assert_eq!(rows[1].pools, ["lanes"]);
    assert!(space.slot_row("slot-x").unwrap().is_some());
    assert!(space.slot_row("nope").unwrap().is_none());
}
