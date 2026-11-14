use vivi_core::pump::flow::index::StepState;

use super::*;
use crate::data::needs::seed::{NeedSeed, PumpSeed, seed_need, seed_pump};
use crate::test_support::{Seeded, seeded};

const NEED: &str = "a1f3c2d0";

/// Run 1 ended in a CTO restart at plan; run 2 is building two slices.
fn seed_flow(store: &Seeded) -> PumpSeed {
    seed_need(store, &NeedSeed::default());
    let pump = seed_pump(store);
    pump.run(NEED, 1, "build", "research", "restarting");
    pump.step(NEED, 1, "sentinel", "t1-sentinel", StepState::Done);
    pump.step_verdict(
        NEED,
        1,
        "research",
        "t1-research",
        StepState::Done,
        Some("failed"),
    );
    pump.step_verdict(NEED, 1, "cto", "t1-cto", StepState::Done, Some("plan"));
    pump.store.set_restart_entry(NEED, 1, "plan").unwrap();

    pump.run(NEED, 2, "build", "plan", "live");
    pump.step(NEED, 2, "sentinel", "t2-sentinel", StepState::Running);
    pump.step_verdict(NEED, 2, "plan", "t2-plan", StepState::Done, Some("planned"));
    pump.step_verdict(
        NEED,
        2,
        "admit1",
        "t2-admit",
        StepState::Done,
        Some("admitted"),
    );
    pump.step_verdict(
        NEED,
        2,
        "slice",
        "t2-slice",
        StepState::Done,
        Some("sliced"),
    );
    pump.step(NEED, 2, "build1", "t2-build", StepState::Running);
    pump.slice(
        NEED,
        2,
        "s1",
        "t2-build",
        &["radix/call.rs", "radix/abi.rs"],
    );
    pump.slice(NEED, 2, "s2", "t2-build", &["runtime/tuple.rs"]);
    pump.store.set_open_fanout(NEED, 2, "t2-build").unwrap();
    pump.store
        .set_fanout_start("t2-build", Some("deadbeefcafe"))
        .unwrap();
    pump.job(NEED, 2, "slice:s1", "task-s1", "job-s1a", 1);
    pump.job(NEED, 2, "slice:s1", "task-s1", "job-s1b", 2);
    pump.rung("task-s1", "task", 1000, Some(1180), Some(0));
    pump.rung("t2-plan", "plan-check", 900, Some(910), Some(2));
    pump.store
        .set_scope_state("task-s2", vivi_core::pump::flow::scope::ScopeState::Wait)
        .unwrap();
    pump.store
        .set_scope_holders(
            "task-s2",
            &["task-s1".to_string(), "path:radix/abi.rs".to_string()],
        )
        .unwrap();
    pump.plan(NEED, 1, "revise", "split abi from call lowering", 800);
    pump.plan(NEED, 2, "planned", "ok", 850);
    pump.branch(NEED, "vivi/need/a1f3c2d0");
    pump
}

fn view(store: &Seeded, run: Option<i64>) -> FlowView {
    let read = match open_pump(&store.root) {
        PumpState::Ready(read) => read,
        other => panic!("pump not ready: {:?}", other.status()),
    };
    read.read(|conn| load_flow(conn, NEED, run)).unwrap()
}

#[test]
fn the_newest_run_is_shown_with_its_path_and_current_step() {
    let store = seeded(true);
    let _pump = seed_flow(&store);
    let flow = view(&store, None);
    assert_eq!(flow.runs.len(), 2);
    assert_eq!(flow.restarts, [(1, "plan".to_string())]);
    let detail = flow.selected.unwrap();
    assert_eq!(detail.run.run, 2);
    let nodes: Vec<&str> = detail.path.iter().map(|e| e.step.node.as_str()).collect();
    assert_eq!(
        nodes,
        ["plan", "admit1", "slice", "build1"],
        "no sentinel, no slices"
    );
    assert_eq!(detail.current, Some(3));
    assert_eq!(detail.path[0].step.verdict.as_deref(), Some("planned"));
    let plan_rung = detail.path[0].rung.as_ref().unwrap();
    assert_eq!(
        (plan_rung.rung.as_str(), plan_rung.rc),
        ("plan-check", Some(2))
    );
}

#[test]
fn an_earlier_run_can_be_picked() {
    let store = seeded(true);
    let _pump = seed_flow(&store);
    let flow = view(&store, Some(1));
    let detail = flow.selected.unwrap();
    assert_eq!(detail.run.entry, "research");
    assert_eq!(detail.path.len(), 2);
    assert!(detail.groups.is_empty());
    assert!(view(&store, Some(9)).selected.is_none());
}

#[test]
fn slices_carry_scope_tries_rung_scope_wait_and_job() {
    let store = seeded(true);
    let _pump = seed_flow(&store);
    let detail = view(&store, None).selected.unwrap();
    assert_eq!(detail.groups.len(), 1);
    let group = &detail.groups[0];
    assert_eq!(group.node, "build1");
    assert!(group.open);
    assert_eq!(group.tip.as_deref(), Some("deadbeefcafe"));
    let s1 = &group.slices[0];
    assert_eq!(s1.id, "s1");
    assert_eq!(s1.scope, ["radix/call.rs", "radix/abi.rs"]);
    assert_eq!(s1.tries, 2);
    assert_eq!(s1.last_job.as_deref(), Some("job-s1b"));
    assert_eq!(s1.rung.as_ref().unwrap().rc, Some(0));
    let s2 = &group.slices[1];
    assert_eq!(s2.scope_state, "scope-wait");
    assert_eq!(s2.holders, ["task-s1"]);
    assert_eq!(s2.held_paths, ["radix/abi.rs"]);
    assert_eq!(s2.tries, 0);
    assert!(s2.rung.is_none());
}

#[test]
fn plan_revisions_and_the_need_branch_are_read() {
    let store = seeded(true);
    let _pump = seed_flow(&store);
    let flow = view(&store, None);
    let verdicts: Vec<(i64, &str)> = flow
        .plans
        .iter()
        .map(|p| (p.revision, p.verdict.as_str()))
        .collect();
    assert_eq!(verdicts, [(1, "revise"), (2, "planned")]);
    assert_eq!(flow.plans[0].reason, "split abi from call lowering");
    let branch = flow.branch.unwrap();
    assert_eq!(branch.branch, "vivi/need/a1f3c2d0");
    assert_eq!(branch.base.as_deref(), Some("abc1234"));
}

#[test]
fn a_need_with_no_runs_has_an_empty_view() {
    let store = seeded(true);
    let _pump = seed_flow(&store);
    let read = match open_pump(&store.root) {
        PumpState::Ready(read) => read,
        _ => panic!("pump not ready"),
    };
    let flow = read.read(|conn| load_flow(conn, "ffffffff", None)).unwrap();
    assert!(flow.runs.is_empty() && flow.selected.is_none() && flow.plans.is_empty());
}
