//! Unit tests for [`super::view`]: the `flow show` view of a need's runs.

use tempfile::tempdir;

use super::def::FlowKind;
use super::index::{RunRow, StepState, run_state};
use super::registry;
use super::view;
use crate::pump::store::PumpStore;

fn build_run(need: &str, run: u32) -> RunRow {
    RunRow {
        need: need.into(),
        run,
        flow: String::from("build"),
        flow_version: 2,
        kind: String::from("build"),
        entry: String::from("plan"),
        state: run_state::LIVE.into(),
    }
}

#[test]
fn view_without_a_need_carries_the_shipped_source_only() {
    let project = tempdir().unwrap();
    let show = view(project.path(), FlowKind::Build, None).unwrap();
    assert_eq!(show.kind, FlowKind::Build);
    assert_eq!(show.mermaid, registry::shipped(FlowKind::Build).unwrap());
    assert_eq!(show.runs, None);
}

#[test]
fn a_need_without_a_pump_db_views_as_none() {
    let project = tempdir().unwrap();
    let show = view(project.path(), FlowKind::Build, Some("abc12345")).unwrap();
    assert_eq!(show.mermaid, registry::shipped(FlowKind::Build).unwrap());
    assert_eq!(show.runs, Some(None));
}

#[test]
fn the_view_lists_a_need_runs_with_their_steps_in_order() {
    let project = tempdir().unwrap();
    let db = project.path().join(".vivi").join("pump").join("pump.db");
    let first = build_run("abc12345", 1);
    let second = build_run("abc12345", 2);
    {
        let store = PumpStore::open(&db).unwrap();
        store.insert_run(&first).unwrap();
        store.insert_run(&second).unwrap();
        store.insert_run(&build_run("other99", 1)).unwrap();
        store
            .insert_step("abc12345", &first.step("plan", "t1", StepState::Done))
            .unwrap();
        let mut slice = first.step("slice", "t2", StepState::Waiting);
        slice.verdict = Some(String::from("split"));
        store.insert_step("abc12345", &slice).unwrap();
        store
            .insert_step("abc12345", &second.step("verify", "t3", StepState::Running))
            .unwrap();
    }

    let show = view(project.path(), FlowKind::Build, Some("abc12345")).unwrap();
    let runs = show
        .runs
        .expect("a need was requested")
        .expect("pump.db exists");
    assert_eq!(runs.len(), 2);
    let (run, steps) = &runs[0];
    assert_eq!(
        (run.need.as_str(), run.run, run.flow.as_str()),
        ("abc12345", 1, "build")
    );
    assert_eq!(steps.len(), 2);
    assert_eq!(
        (
            steps[0].node.as_str(),
            steps[0].task.as_str(),
            steps[0].state
        ),
        ("plan", "t1", StepState::Done)
    );
    assert_eq!(steps[0].verdict, None);
    assert_eq!(steps[1].verdict.as_deref(), Some("split"));
    let (run, steps) = &runs[1];
    assert_eq!((run.need.as_str(), run.run), ("abc12345", 2));
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].task, "t3");
}
