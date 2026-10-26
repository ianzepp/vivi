//! Origin-resolution tests for [`super::origin`]: the topology walk over the
//! shipped build flow, a synthetic flow without any planner, and the
//! recorded-link rules.

use tempfile::{TempDir, tempdir};

use super::origin;
use crate::pump::flow::def::{FlowDef, FlowEdge, FlowKind, FlowNode, NodeClass};
use crate::pump::flow::index::{RunRow, StepRow, StepState, run_state};
use crate::pump::flow::load::load;
use crate::pump::store::flow_continuation::ContinuationLink;
use crate::pump::store::{DB_FILENAME, PumpStore};

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

fn rig(dir: &TempDir, need: &str, run: u32) -> PumpStore {
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store.insert_run(&run_row(need, run)).expect("insert run");
    store
}

fn step(run: &RunRow, node: &str, task: &str, state: StepState, verdict: Option<&str>) -> StepRow {
    let mut step = run.step(node, task, state);
    step.verdict = verdict.map(String::from);
    step
}

/// The park step under test: waiting on node `parked` of the build flow.
fn park_step(run: &RunRow, task: &str) -> StepRow {
    run.step("parked", task, StepState::Waiting)
}

fn link(need: &str, run: u32, park_task: &str, source: &str) -> ContinuationLink {
    ContinuationLink {
        task: park_task.into(),
        need: need.into(),
        run,
        node: "parked".into(),
        source: source.into(),
    }
}

/// A two-entry flow with no planner at all: a researcher and an auditor
/// park the run through their own decisions, the auditor through a chain of
/// two decision nodes.
fn synthetic_def() -> FlowDef {
    let node = |id: &str, class: NodeClass| FlowNode {
        id: id.into(),
        class,
        label: id.into(),
    };
    let edge = |from: &str, to: &str, words: &[&str]| FlowEdge {
        from: from.into(),
        to: to.into(),
        words: words.iter().map(|w| (*w).to_string()).collect(),
    };
    FlowDef {
        kind: FlowKind::Repair,
        nodes: vec![
            node("probe", NodeClass::Researcher),
            node("pq", NodeClass::Decision),
            node("audit", NodeClass::Auditor),
            node("aq", NodeClass::Decision),
            node("hq", NodeClass::Decision),
            node("parked", NodeClass::Park),
        ],
        edges: vec![
            edge("probe", "pq", &[]),
            edge("pq", "parked", &["NEEDS FURTHER REVIEW", "NOT READY"]),
            edge("audit", "aq", &[]),
            edge("aq", "hq", &["residual"]),
            edge("hq", "parked", &["residual", "block_range"]),
        ],
        entries: vec!["probe".into(), "audit".into()],
    }
}

#[test]
fn plan_verdict_resolves_through_the_decision() {
    let dir = tempdir().expect("tempdir");
    let store = rig(&dir, "n1", 1);
    let run = run_row("n1", 1);
    let def = load(FlowKind::Build).expect("load build flow");
    for row in [
        step(&run, "plan", "t-plan", StepState::Done, Some("operator")),
        park_step(&run, "t-park"),
    ] {
        store.insert_run_step(&run, &row).expect("insert step");
    }
    let park = park_step(&run, "t-park");
    let found = origin(&def, &store, &run, &park).expect("origin");
    assert_eq!(found, "t-plan");
}

#[test]
fn walk_follows_the_actual_verdict_to_the_later_plan() {
    let dir = tempdir().expect("tempdir");
    let store = rig(&dir, "n1", 1);
    let run = run_row("n1", 1);
    let def = load(FlowKind::Build).expect("load build flow");
    for row in [
        step(&run, "plan", "t-plan", StepState::Done, Some("planned")),
        step(&run, "admit1", "t-admit", StepState::Done, Some("revise")),
        step(&run, "plan2", "t-plan2", StepState::Done, Some("hold")),
        park_step(&run, "t-park"),
    ] {
        store.insert_run_step(&run, &row).expect("insert step");
    }
    let park = park_step(&run, "t-park");
    let found = origin(&def, &store, &run, &park).expect("origin");
    assert_eq!(found, "t-plan2");
}

#[test]
fn two_routing_verdicts_are_ambiguous() {
    let dir = tempdir().expect("tempdir");
    let store = rig(&dir, "n1", 1);
    let run = run_row("n1", 1);
    let def = load(FlowKind::Build).expect("load build flow");
    for row in [
        step(&run, "plan", "t-plan", StepState::Done, Some("hold")),
        step(&run, "plan2", "t-plan2", StepState::Done, Some("operator")),
        park_step(&run, "t-park"),
    ] {
        store.insert_run_step(&run, &row).expect("insert step");
    }
    let park = park_step(&run, "t-park");
    let error = origin(&def, &store, &run, &park).expect_err("ambiguous");
    assert!(error.to_string().contains("ambiguous"), "actual: {error}");
}

#[test]
fn missing_evidence_is_an_error() {
    let dir = tempdir().expect("tempdir");
    let store = rig(&dir, "n1", 1);
    let run = run_row("n1", 1);
    let def = load(FlowKind::Build).expect("load build flow");
    let park = park_step(&run, "t-park");
    store.insert_run_step(&run, &park).expect("insert step");
    let empty = origin(&def, &store, &run, &park).expect_err("no steps");
    assert!(
        empty.to_string().contains("no completed"),
        "actual: {empty}"
    );

    // A completed plan whose verdict does not route to the park is no
    // evidence either, even though the run later holds a park row.
    let plan = step(&run, "plan", "t-plan", StepState::Done, Some("planned"));
    store.insert_run_step(&run, &plan).expect("insert step");
    let unrouted = origin(&def, &store, &run, &park).expect_err("no route");
    assert!(
        unrouted.to_string().contains("no completed"),
        "actual: {unrouted}"
    );
}

#[test]
fn recorded_link_wins_over_the_topology_walk() {
    let dir = tempdir().expect("tempdir");
    let store = rig(&dir, "n1", 1);
    let run = run_row("n1", 1);
    let def = load(FlowKind::Build).expect("load build flow");
    for row in [
        step(&run, "plan", "t-plan", StepState::Done, Some("planned")),
        step(&run, "admit1", "t-admit", StepState::Done, Some("revise")),
        step(&run, "plan2", "t-plan2", StepState::Done, Some("hold")),
        park_step(&run, "t-park"),
    ] {
        store.insert_run_step(&run, &row).expect("insert step");
    }
    // The walk alone would answer t-plan2; the recorded link names t-plan.
    store
        .link_continuation(&link("n1", 1, "t-park", "t-plan"))
        .expect("link");
    let park = park_step(&run, "t-park");
    let found = origin(&def, &store, &run, &park).expect("origin");
    assert_eq!(found, "t-plan");
}

#[test]
fn link_to_another_need_or_run_is_invalid() {
    let dir = tempdir().expect("tempdir");
    let store = rig(&dir, "n1", 1);
    let run = run_row("n1", 1);
    let def = load(FlowKind::Build).expect("load build flow");
    for row in [
        step(&run, "plan", "t-plan", StepState::Done, Some("operator")),
        park_step(&run, "t-park"),
    ] {
        store.insert_run_step(&run, &row).expect("insert step");
    }
    store
        .link_continuation(&link("n1", 1, "t-park", "t-plan"))
        .expect("link");
    let park = park_step(&run, "t-park");
    let other_need = origin(&def, &store, &run_row("n2", 1), &park).expect_err("need");
    assert!(
        other_need.to_string().contains("need n2 r1"),
        "actual: {other_need}"
    );
    let other_run = origin(&def, &store, &run_row("n1", 2), &park).expect_err("run");
    assert!(
        other_run.to_string().contains("need n1 r2"),
        "actual: {other_run}"
    );
}

#[test]
fn link_to_a_non_executable_source_is_invalid() {
    let dir = tempdir().expect("tempdir");
    let store = rig(&dir, "n1", 1);
    let run = run_row("n1", 1);
    let def = load(FlowKind::Build).expect("load build flow");
    for row in [
        step(&run, "plan", "t-plan", StepState::Done, Some("operator")),
        park_step(&run, "t-park"),
        run.step("locked", "t-lock", StepState::Done),
    ] {
        store.insert_run_step(&run, &row).expect("insert step");
    }
    store
        .link_continuation(&link("n1", 1, "t-park", "t-lock"))
        .expect("link stores; the resolver rejects the class");
    let park = park_step(&run, "t-park");
    let error = origin(&def, &store, &run, &park).expect_err("non-executable");
    assert!(
        error.to_string().contains("not an executable node"),
        "actual: {error}"
    );
}

#[test]
fn resolves_generic_executable_origins_without_a_planner() {
    let dir = tempdir().expect("tempdir");
    let store = rig(&dir, "n1", 1);
    let def = synthetic_def();
    let first = run_row("n1", 1);
    for row in [
        step(
            &first,
            "probe",
            "t-probe",
            StepState::Done,
            Some("NEEDS FURTHER REVIEW"),
        ),
        step(
            &first,
            "audit",
            "t-audit",
            StepState::Done,
            Some("clean_pass"),
        ),
        park_step(&first, "t-park"),
    ] {
        store.insert_run_step(&first, &row).expect("insert step");
    }
    let park = park_step(&first, "t-park");
    let found = origin(&def, &store, &first, &park).expect("origin");
    assert_eq!(found, "t-probe");

    // A second run parks through the auditor chain instead: two decision
    // nodes deep, and not a planner class anywhere.
    let second = run_row("n1", 2);
    store.insert_run(&second).expect("insert run");
    for row in [
        step(
            &second,
            "probe",
            "t-probe-2",
            StepState::Done,
            Some("READY"),
        ),
        step(
            &second,
            "audit",
            "t-audit-2",
            StepState::Done,
            Some("residual"),
        ),
        park_step(&second, "t-park-2"),
    ] {
        store.insert_run_step(&second, &row).expect("insert step");
    }
    let park = park_step(&second, "t-park-2");
    let found = origin(&def, &store, &second, &park).expect("origin");
    assert_eq!(found, "t-audit-2");
}
