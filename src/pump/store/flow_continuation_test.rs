//! Tests for the continuation table in [`super`].

use tempfile::{TempDir, tempdir};

use super::ContinuationLink;
use crate::pump::flow::index::{RunRow, StepState, run_state};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

fn run_row(need: &str, run: u32) -> RunRow {
    RunRow {
        need: need.into(),
        run,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "research".into(),
        state: run_state::LIVE.into(),
    }
}

/// One run with a plan step (done, verdict `operator`), a second plan step
/// (done, verdict `hold`) and the park step (waiting).
fn seed(store: &PumpStore, need: &str, run: u32, tasks: [&str; 3]) {
    let row = run_row(need, run);
    store.insert_run(&row).expect("insert run");
    let mut plan = row.step("plan", tasks[0], StepState::Done);
    plan.verdict = Some("operator".into());
    let mut plan2 = row.step("plan 2", tasks[1], StepState::Done);
    plan2.verdict = Some("hold".into());
    let park = row.step("parked", tasks[2], StepState::Waiting);
    for step in [plan, plan2, park] {
        store.insert_run_step(&row, &step).expect("insert step");
    }
}

fn link(need: &str, run: u32, park_task: &str, node: &str, source: &str) -> ContinuationLink {
    ContinuationLink {
        task: park_task.into(),
        need: need.into(),
        run,
        node: node.into(),
        source: source.into(),
    }
}

#[test]
fn link_round_trips_and_reloads() {
    let dir = tempdir().expect("tempdir");
    {
        let store = open(&dir);
        seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
        let link = link("n1", 1, "t-park", "parked", "t-plan");
        store.link_continuation(&link).expect("link");
        assert_eq!(store.continuation_of("t-park").expect("read"), Some(link));
        assert_eq!(store.continuation_of("t-plan").expect("read"), None);
    }
    let reopened = open(&dir);
    assert_eq!(
        reopened.continuation_of("t-park").expect("read"),
        Some(link("n1", 1, "t-park", "parked", "t-plan"))
    );
}

#[test]
fn repeated_identical_link_is_a_no_op() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
    let link = link("n1", 1, "t-park", "parked", "t-plan");
    store.link_continuation(&link).expect("first link");
    store.link_continuation(&link).expect("same link again");
    assert_eq!(store.continuation_of("t-park").expect("read"), Some(link));
}

#[test]
fn second_different_source_is_refused() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
    store
        .link_continuation(&link("n1", 1, "t-park", "parked", "t-plan"))
        .expect("first link");
    let conflict = store.link_continuation(&link("n1", 1, "t-park", "parked", "t-plan2"));
    assert!(conflict.is_err(), "conflicting link must fail");
    assert_eq!(
        store.continuation_of("t-park").expect("read"),
        Some(link("n1", 1, "t-park", "parked", "t-plan"))
    );
}

#[test]
fn link_needs_the_park_step_in_its_need_and_run() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
    let missing = store.link_continuation(&link("n1", 1, "t-none", "parked", "t-plan"));
    assert!(missing.is_err(), "unknown park task must fail");
    let other_node = store.link_continuation(&link("n1", 1, "t-park", "park", "t-plan"));
    assert!(other_node.is_err(), "wrong node must fail");
    let other_need = store.link_continuation(&link("n2", 1, "t-park", "parked", "t-plan"));
    assert!(other_need.is_err(), "park of another need must fail");
    let other_run = store.link_continuation(&link("n1", 2, "t-park", "parked", "t-plan"));
    assert!(other_run.is_err(), "park of another run must fail");
    assert_eq!(store.continuation_of("t-park").expect("read"), None);
}

#[test]
fn link_needs_the_source_step_in_the_same_need_and_run() {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    seed(&store, "n1", 1, ["t-plan", "t-plan2", "t-park"]);
    seed(&store, "n1", 2, ["t-plan-r2", "t-plan2-r2", "t-park-r2"]);
    seed(&store, "n2", 1, ["t-plan-n2", "t-plan2-n2", "t-park-n2"]);
    let ghost = store.link_continuation(&link("n1", 1, "t-park", "parked", "t-ghost"));
    assert!(ghost.is_err(), "unknown source must fail");
    let other_run = store.link_continuation(&link("n1", 1, "t-park", "parked", "t-plan-r2"));
    assert!(other_run.is_err(), "source of another run must fail");
    let other_need = store.link_continuation(&link("n1", 1, "t-park", "parked", "t-plan-n2"));
    assert!(other_need.is_err(), "source of another need must fail");
    assert_eq!(store.continuation_of("t-park").expect("read"), None);
}
