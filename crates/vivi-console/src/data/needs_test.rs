use vivi_core::pump::flow::index::StepState;

use super::seed::{NeedSeed, seed_need, seed_pump};
use super::*;
use crate::test_support::{SeedRun, project_without_store, seeded};

const NOW: i64 = 1_800_000_000;

fn two_open_one_done(store: &crate::test_support::Seeded) {
    seed_need(store, &NeedSeed::default());
    seed_need(
        store,
        &NeedSeed {
            handle: "5c20e9b7",
            subject: "parser: trailing commas",
            created_at: "2026-11-14T09:00:00+00:00",
            ..NeedSeed::default()
        },
    );
    seed_need(
        store,
        &NeedSeed {
            handle: "9e04a6f1",
            subject: "old finished work",
            state: "done",
            created_at: "2026-11-13T09:00:00+00:00",
            ..NeedSeed::default()
        },
    );
}

#[test]
fn the_list_is_newest_first_and_filtered_by_state() {
    let store = seeded(true);
    two_open_one_done(&store);
    let open = load(&store.root, NOW, StateFilter::Open).unwrap().unwrap();
    let handles: Vec<&str> = open.rows.iter().map(|r| r.node.handle.as_str()).collect();
    assert_eq!(handles, ["5c20e9b7", "a1f3c2d0"]);
    assert!(!open.truncated);
    let done = load(&store.root, NOW, StateFilter::Done).unwrap().unwrap();
    assert_eq!(done.rows.len(), 1);
    assert_eq!(done.rows[0].node.subject, "old finished work");
    let any = load(&store.root, NOW, StateFilter::Any).unwrap().unwrap();
    assert_eq!(any.rows.len(), 3);
}

#[test]
fn a_need_counts_its_attempts_and_shows_the_newest() {
    let store = seeded(true);
    two_open_one_done(&store);
    let base = SeedRun::default;
    store.insert(&SeedRun {
        job: "job-1",
        need: Some("a1f3c2d0"),
        dispatched_at: 100,
        collected_at: Some(900),
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(1),
        ..base()
    });
    store.insert(&SeedRun {
        job: "job-2",
        need: Some("a1f3c2d0"),
        dispatched_at: 2000,
        kind: Some("rework"),
        ..base()
    });
    store.insert(&SeedRun {
        job: "job-other",
        need: Some("5c20e9b7"),
        dispatched_at: 50,
        ..base()
    });
    let data = load(&store.root, NOW, StateFilter::Open).unwrap().unwrap();
    let a1 = data
        .rows
        .iter()
        .find(|r| r.node.handle == "a1f3c2d0")
        .unwrap();
    let attempts = a1.attempts.as_ref().unwrap();
    assert_eq!(attempts.count, 2);
    assert_eq!(attempts.newest[0].run.job, "job-2");
    assert_eq!(a1.kind(), Some("rework"));
    let other = data
        .rows
        .iter()
        .find(|r| r.node.handle == "5c20e9b7")
        .unwrap();
    assert_eq!(other.attempts.as_ref().unwrap().count, 1);
}

#[test]
fn a_store_without_a_slot_index_still_lists_needs() {
    let store = seeded(false);
    two_open_one_done(&store);
    let data = load(&store.root, NOW, StateFilter::Open).unwrap().unwrap();
    assert_eq!(data.rows.len(), 2);
    assert!(data.rows.iter().all(|row| row.attempts.is_none()));
}

#[test]
fn a_project_without_a_store_has_no_list() {
    let (_dir, root) = project_without_store();
    assert!(load(&root, NOW, StateFilter::Open).unwrap().is_none());
}

#[test]
fn the_pump_state_is_named() {
    let store = seeded(true);
    two_open_one_done(&store);
    let data = load(&store.root, NOW, StateFilter::Open).unwrap().unwrap();
    assert_eq!(data.pump, PumpState::NoPump);
    let pump = seed_pump(&store);
    let data = load(&store.root, NOW, StateFilter::Open).unwrap().unwrap();
    assert_eq!(data.pump, PumpState::Ready(()));
    drop(pump);
    let shm = store.root.join(".vivi/pump/pump.db-shm");
    if shm.exists() {
        return; // the platform left the WAL index behind: nothing to refuse
    }
    let data = load(&store.root, NOW, StateFilter::Open).unwrap().unwrap();
    assert!(
        matches!(data.pump, PumpState::Unreadable(_)),
        "{:?}",
        data.pump
    );
    assert!(!shm.exists(), "the console must not create the index file");
    assert_eq!(data.rows.len(), 2, "the list still reads without the pump");
}

#[test]
fn a_configured_pump_with_no_store_is_not_started() {
    let store = seeded(true);
    two_open_one_done(&store);
    let config = store.root.join(".vivi/mailspace.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("{text}\n[pump]\n")).unwrap();
    let data = load(&store.root, NOW, StateFilter::Open).unwrap().unwrap();
    assert_eq!(data.pump, PumpState::NotStarted);
}

#[test]
fn the_flow_position_is_joined_by_the_need_handle() {
    let store = seeded(true);
    two_open_one_done(&store);
    let pump = seed_pump(&store);
    pump.run("a1f3c2d0", 1, "build", "plan", "live");
    pump.step("a1f3c2d0", 1, "sentinel", "t-sentinel", StepState::Running);
    pump.step_verdict(
        "a1f3c2d0",
        1,
        "plan",
        "t-plan",
        StepState::Done,
        Some("planned"),
    );
    pump.step("a1f3c2d0", 1, "build1", "t-build", StepState::Running);
    pump.step("a1f3c2d0", 1, "slice:s1", "t-s1", StepState::Running);
    pump.job("a1f3c2d0", 1, "build1", "t-build", "job-step", 1);
    store.insert(&SeedRun {
        job: "job-step",
        need: Some("a1f3c2d0"),
        dispatched_at: NOW - 7200,
        ..SeedRun::default()
    });
    let data = load(&store.root, NOW, StateFilter::Open).unwrap().unwrap();
    let a1 = data
        .rows
        .iter()
        .find(|r| r.node.handle == "a1f3c2d0")
        .unwrap();
    let flow = a1.flow.as_ref().unwrap();
    assert_eq!(flow.run.run, 1);
    assert_eq!(a1.kind(), Some("build"));
    assert_eq!(a1.step(), Some("build1"));
    assert_eq!(flow.step_since, Some(NOW - 7200));
    let other = data
        .rows
        .iter()
        .find(|r| r.node.handle == "5c20e9b7")
        .unwrap();
    assert!(other.flow.is_none());
}
