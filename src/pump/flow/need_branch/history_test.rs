//! The cross-run slice inventory over the flow index tables.

use tempfile::{TempDir, tempdir};

use super::{SliceInventory, inventory};
use crate::pump::flow::def::VERDICT_REPLACED;
use crate::pump::flow::fanout::{VERDICT_MERGED, VERDICT_NO_DEFECT};
use crate::pump::flow::index::{RunRow, StepRow, StepState, run_state};
use crate::pump::store::{DB_FILENAME, PumpStore};

const NEED: &str = "abcd1234";
const MERGED: Option<&str> = Some(VERDICT_MERGED);
const NO_DEFECT: Option<&str> = Some(VERDICT_NO_DEFECT);

struct Fixture {
    _dir: TempDir,
    store: PumpStore,
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture { _dir: dir, store }
}

fn run(n: u32, state: &str) -> RunRow {
    RunRow {
        need: NEED.into(),
        run: n,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "admit".into(),
        state: state.into(),
    }
}

fn slice(n: u32, id: &str, task: &str, state: StepState, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: n,
        node: format!("slice:{id}"),
        task: task.into(),
        state,
        verdict: verdict.map(str::to_string),
    }
}

fn flow_step(n: u32, node: &str, task: &str) -> StepRow {
    StepRow {
        run: n,
        node: node.into(),
        task: task.into(),
        state: StepState::Done,
        verdict: Some(VERDICT_MERGED.into()),
    }
}

fn entry(run: u32, id: &str, task: &str, verdict: &str) -> (u32, String, String, String) {
    (run, format!("slice:{id}"), task.into(), verdict.into())
}

#[test]
fn the_inventory_spans_three_runs_in_deterministic_order() {
    let f = fixture();
    f.store.insert_run(&run(1, run_state::DONE)).unwrap();
    f.store.insert_run(&run(2, run_state::SUPERSEDED)).unwrap();
    f.store.insert_run(&run(3, run_state::LIVE)).unwrap();
    // Run 1: two completions, including a node reused later by another run.
    f.store
        .insert_step(NEED, &slice(1, "s1", "t1", StepState::Done, MERGED))
        .unwrap();
    f.store
        .insert_step(NEED, &slice(1, "s2", "t2", StepState::Done, NO_DEFECT))
        .unwrap();
    // Run 2: the same node name with a distinct task, and a replaced slice.
    f.store
        .insert_step(NEED, &slice(2, "s1", "t3", StepState::Done, NO_DEFECT))
        .unwrap();
    f.store
        .insert_step(
            NEED,
            &slice(2, "s3", "t4", StepState::Done, Some(VERDICT_REPLACED)),
        )
        .unwrap();
    // Run 3: a completion out of insertion order, so the sort is visible.
    f.store
        .insert_step(NEED, &slice(3, "s5", "t6", StepState::Done, MERGED))
        .unwrap();
    f.store
        .insert_step(NEED, &slice(3, "s4", "t5", StepState::Done, MERGED))
        .unwrap();

    let got = inventory(&f.store, NEED).unwrap();
    let entries: Vec<_> = got
        .entries
        .iter()
        .map(|e| (e.run, e.node.clone(), e.task.clone(), e.verdict.clone()))
        .collect();
    assert_eq!(
        entries,
        vec![
            entry(1, "s1", "t1", VERDICT_MERGED),
            entry(1, "s2", "t2", VERDICT_NO_DEFECT),
            entry(2, "s1", "t3", VERDICT_NO_DEFECT),
            entry(3, "s4", "t5", VERDICT_MERGED),
            entry(3, "s5", "t6", VERDICT_MERGED),
        ]
    );
    assert_eq!(got.merged(), 3);
    assert_eq!(got.no_defect(), 2);
}

#[test]
fn failed_exhausted_unfinished_and_non_slice_rows_are_excluded() {
    let f = fixture();
    f.store.insert_run(&run(1, run_state::SUPERSEDED)).unwrap();
    f.store.insert_run(&run(2, run_state::LIVE)).unwrap();
    let excluded = [
        slice(1, "s1", "t1", StepState::Failed, None),
        slice(1, "s2", "t2", StepState::Failed, MERGED),
        slice(1, "s3", "t3", StepState::Pending, MERGED),
        slice(2, "s4", "t4", StepState::Running, MERGED),
        slice(2, "s5", "t5", StepState::Waiting, MERGED),
        slice(2, "s6", "t6", StepState::Done, Some(VERDICT_REPLACED)),
        slice(2, "s7", "t7", StepState::Done, None),
        flow_step(2, "admit", "t8"),
    ];
    for row in &excluded {
        f.store.insert_step(NEED, row).unwrap();
    }

    let got = inventory(&f.store, NEED).unwrap();
    assert_eq!(got, SliceInventory::default());
    assert_eq!(got.merged(), 0);
    assert_eq!(got.no_defect(), 0);
}

#[test]
fn completions_from_superseded_runs_stay_as_historical_claims() {
    let f = fixture();
    f.store.insert_run(&run(1, run_state::SUPERSEDED)).unwrap();
    f.store.insert_run(&run(2, run_state::LIVE)).unwrap();
    f.store
        .insert_step(NEED, &slice(1, "s1", "t1", StepState::Done, MERGED))
        .unwrap();
    f.store
        .insert_step(NEED, &slice(2, "s2", "t2", StepState::Done, MERGED))
        .unwrap();

    let got = inventory(&f.store, NEED).unwrap();
    // The run-1 completion is retained even though its run is superseded.
    let historical: Vec<_> = got.historical(2).map(|e| e.task.as_str()).collect();
    assert_eq!(historical, vec!["t1"]);
    // Its provenance is run-qualified, so a caller can tell the claim from
    // work on the live branch — the prior archive proves nothing about HEAD.
    let old = got.entries.iter().find(|e| e.task == "t1").unwrap();
    assert_eq!(old.run, 1);
    assert_ne!(old.run, 2);
    assert!(got.in_run(2).all(|e| e.task != "t1"));
    assert_eq!(
        got.in_run(2).map(|e| e.task.as_str()).collect::<Vec<_>>(),
        vec!["t2"]
    );
    // The claim still counts as a merged completion.
    assert_eq!(got.merged(), 2);
}

#[test]
fn an_unknown_need_has_an_empty_inventory() {
    let f = fixture();
    assert_eq!(
        inventory(&f.store, NEED).unwrap(),
        SliceInventory::default()
    );
    assert_eq!(inventory(&f.store, NEED).unwrap().historical(1).count(), 0);
    assert_eq!(inventory(&f.store, NEED).unwrap().in_run(1).count(), 0);
}
