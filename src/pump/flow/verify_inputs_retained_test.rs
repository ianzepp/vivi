//! Tests for the retained-work readers in [`super`].

use tempfile::{TempDir, tempdir};

use super::{RetainedSlice, retained_work};
use crate::pump::flow::fanout::slice_node;
use crate::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, StepRow, StepState, run_state};
use crate::pump::flow::need_branch;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

const NEED: &str = "need-1";

struct Fixture {
    _dir: TempDir,
    store: PumpStore,
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture { _dir: dir, store }
}

fn run(number: u32, state: &str) -> RunRow {
    RunRow {
        need: NEED.into(),
        run: number,
        flow: "build".into(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: "build".into(),
        entry: "start".into(),
        state: state.into(),
    }
}

fn slice(number: u32, id: &str, task: &str, state: StepState, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: number,
        node: slice_node(id),
        task: task.into(),
        state,
        verdict: verdict.map(str::to_string),
    }
}

fn record(f: &Fixture, step: &StepRow) {
    f.store.insert_step(NEED, step).expect("insert step");
}

fn entry(number: u32, id: &str, task: &str) -> RetainedSlice {
    RetainedSlice {
        run: number,
        slice: id.into(),
        task: task.into(),
    }
}

#[test]
fn retained_work_reads_a_three_run_continuation() {
    let f = fixture();
    for number in 1..=3 {
        f.store.insert_run(&run(number, run_state::DONE)).unwrap();
    }
    record(
        &f,
        &slice(1, "aaaa1111-1", "t1", StepState::Done, Some("merged")),
    );
    record(
        &f,
        &slice(2, "bbbb2222-1", "t2", StepState::Done, Some("no-defect")),
    );
    record(
        &f,
        &slice(3, "cccc3333-2", "t3", StepState::Done, Some("merged")),
    );
    let work = retained_work(&f.store, NEED).unwrap();
    assert_eq!(
        work.merged,
        vec![entry(1, "aaaa1111-1", "t1"), entry(3, "cccc3333-2", "t3")]
    );
    assert_eq!(work.no_defect, vec![entry(2, "bbbb2222-1", "t2")]);
    // No priors and no row: every run is of the active generation.
    assert_eq!(work.active_merged(), work.merged.as_slice());
    assert_eq!(work.active_no_defect(), work.no_defect.as_slice());
}

#[test]
fn completed_merges_survive_a_superseded_run() {
    let f = fixture();
    f.store.insert_run(&run(1, run_state::DONE)).unwrap();
    f.store.insert_run(&run(2, run_state::SUPERSEDED)).unwrap();
    f.store.insert_run(&run(3, run_state::LIVE)).unwrap();
    record(
        &f,
        &slice(1, "aaaa1111-1", "t1", StepState::Done, Some("no-defect")),
    );
    record(
        &f,
        &slice(2, "bbbb2222-9", "t2", StepState::Done, Some("merged")),
    );
    let work = retained_work(&f.store, NEED).unwrap();
    assert_eq!(work.merged, vec![entry(2, "bbbb2222-9", "t2")]);
    assert_eq!(work.active_merged(), work.merged.as_slice());
    assert_eq!(work.no_defect, vec![entry(1, "aaaa1111-1", "t1")]);
    assert_eq!(work.active_no_defect(), work.no_defect.as_slice());
}

#[test]
fn retained_entries_keep_run_slice_and_task_identity() {
    let f = fixture();
    f.store.insert_run(&run(1, run_state::DONE)).unwrap();
    record(
        &f,
        &slice(1, "ffff0001-1", "task-a", StepState::Done, Some("merged")),
    );
    record(
        &f,
        &slice(
            1,
            "ffff0001-2",
            "task-b",
            StepState::Done,
            Some("no-defect"),
        ),
    );
    let work = retained_work(&f.store, NEED).unwrap();
    assert_eq!(work.merged, vec![entry(1, "ffff0001-1", "task-a")]);
    assert_eq!(work.no_defect, vec![entry(1, "ffff0001-2", "task-b")]);
}

#[test]
fn all_non_completion_outcomes_are_excluded() {
    let f = fixture();
    f.store.insert_run(&run(1, run_state::DONE)).unwrap();
    record(&f, &slice(1, "aaaa1111-1", "t1", StepState::Pending, None));
    record(&f, &slice(1, "aaaa1111-2", "t2", StepState::Running, None));
    record(&f, &slice(1, "aaaa1111-3", "t3", StepState::Waiting, None));
    record(
        &f,
        &slice(1, "aaaa1111-4", "t4", StepState::Done, Some("failed")),
    );
    record(
        &f,
        &slice(1, "aaaa1111-5", "t5", StepState::Failed, Some("failed")),
    );
    record(
        &f,
        &slice(1, "aaaa1111-6", "t6", StepState::Done, Some("replaced")),
    );
    record(
        &f,
        &slice(1, "aaaa1111-7", "t7", StepState::Done, Some("superseded")),
    );
    record(
        &f,
        &slice(1, "aaaa1111-8", "t8", StepState::Done, Some("withdrawn")),
    );
    record(&f, &slice(1, "aaaa1111-9", "t9", StepState::Done, None));
    let work = retained_work(&f.store, NEED).unwrap();
    assert_eq!(work.merged, vec![]);
    assert_eq!(work.no_defect, vec![]);
    assert_eq!(work.active_merged(), work.merged.as_slice());
    assert_eq!(work.active_no_defect(), work.no_defect.as_slice());
}

#[test]
fn no_defect_closes_stay_out_of_the_merged_collection() {
    let f = fixture();
    f.store.insert_run(&run(1, run_state::DONE)).unwrap();
    record(
        &f,
        &slice(1, "aaaa1111-1", "t1", StepState::Done, Some("merged")),
    );
    record(
        &f,
        &slice(1, "aaaa1111-2", "t2", StepState::Done, Some("no-defect")),
    );
    let work = retained_work(&f.store, NEED).unwrap();
    assert_eq!(work.merged.len(), 1);
    assert_eq!(work.merged[0].task, "t1");
    assert_eq!(work.no_defect.len(), 1);
    assert_eq!(work.no_defect[0].task, "t2");
}

#[test]
fn merges_from_a_reset_generation_never_prove_active_integration() {
    let f = fixture();
    for number in 1..=4 {
        f.store.insert_run(&run(number, run_state::DONE)).unwrap();
    }
    for (number, task) in [(1, "t1"), (2, "t2"), (3, "t3"), (4, "t4")] {
        record(
            &f,
            &slice(number, "eeee4444-1", task, StepState::Done, Some("merged")),
        );
    }
    // Two archived generations: the branch was reset after run 1 and again
    // after run 2, so only runs 3 and 4 sit on the active branch.
    f.store
        .upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: need_branch::branch(NEED),
            base: Some("1111111111111111111111111111111111111111".into()),
            status: "live".into(),
            run: 4,
            target: "default".into(),
            priors: vec![need_branch::prior(NEED, 1), need_branch::prior(NEED, 2)],
        })
        .unwrap();
    let work = retained_work(&f.store, NEED).unwrap();
    // The full collection keeps every generation for the checked-tip reader.
    assert_eq!(
        work.merged,
        vec![
            entry(1, "eeee4444-1", "t1"),
            entry(2, "eeee4444-1", "t2"),
            entry(3, "eeee4444-1", "t3"),
            entry(4, "eeee4444-1", "t4"),
        ]
    );
    assert_eq!(
        work.active_merged(),
        &[entry(3, "eeee4444-1", "t3"), entry(4, "eeee4444-1", "t4")]
    );
    assert_eq!(work.active_no_defect(), work.no_defect.as_slice());
}
