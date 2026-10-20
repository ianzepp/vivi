use std::fs;

use super::*;
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus};
use crate::pump::state::State;

fn job(receipt: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(
        "planner-board-need1-100",
        "planner:board",
        "planner",
        "board planner need1",
        "plan",
    ));
    job.queued_at = 100;
    job.pool_candidates = vec!["here".into()];
    job.read_only = true;
    job.payload = serde_json::json!({
        "planner_output": { "out": "board/need1.out", "plan": "plans/need1.json" }
    });
    job.task = Some("task0001".into());
    job.pool = Some("here".into());
    job.dispatched_at = Some(100);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: "agent_rc 0".into(),
        receipt: receipt.into(),
        at: 101,
    });
    job
}

#[test]
fn a_completed_plan_is_copied_and_its_receipt_is_kept_for_ingestion() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let receipt = "agent_rc 0 agent_s 1\nhand's final message:\nPUMP-PLAN-BEGIN\n{\"verdict\":\"none\"}\nPUMP-PLAN-END\n";
    let mut st = State::default();
    let job = job(receipt);
    let id = job.id.clone();
    st.jobs.insert(id.clone(), job);
    apply_completions(&ctx, &mut st);

    assert!(st.jobs[&id].applied_at.is_some());
    assert_eq!(
        fs::read_to_string(ctx.home.join("plans/need1.json")).unwrap(),
        "{\"verdict\":\"none\"}"
    );
    assert_eq!(
        fs::read_to_string(ctx.home.join("board/need1.out")).unwrap(),
        receipt
    );
    assert_eq!(
        fs::read_to_string(ctx.home.join("board/need1.out.rc")).unwrap(),
        "0\n"
    );
}

#[test]
fn an_immediate_worker_exit_writes_the_failure_receipt_on_the_same_tick() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let receipt = "agent_rc 127 agent_s 0\nhand's final message:\nbash: pi: command not found\n";
    let mut st = State::default();
    let job = job(receipt);
    let id = job.id.clone();
    st.jobs.insert(id.clone(), job);
    apply_completions(&ctx, &mut st);

    assert!(st.jobs[&id].applied_at.is_some());
    assert!(!ctx.home.join("plans/need1.json").exists());
    assert_eq!(
        fs::read_to_string(ctx.home.join("board/need1.out.rc")).unwrap(),
        "127\n"
    );
    assert!(
        fs::read_to_string(ctx.home.join("board/need1.out"))
            .unwrap()
            .contains("bash: pi: command not found")
    );
}

fn planner_routing(roles: &[(&str, &str)]) -> crate::routing::RoleRouting {
    use crate::routing::{RoleRouting, RoutedPool, RoutedRole};
    RoleRouting {
        roles: roles
            .iter()
            .map(|(name, pool)| RoutedRole {
                name: (*name).into(),
                kind: Some("planner".into()),
                status: "active".into(),
                pool: Some((*pool).into()),
                read_only: None,
                model: None,
                models: Vec::new(),
            })
            .collect(),
        pools: roles
            .iter()
            .map(|(_, pool)| {
                (
                    (*pool).to_string(),
                    RoutedPool {
                        slots: 1,
                        provider: "local".into(),
                        fallback: None,
                        caps: std::collections::BTreeMap::new(),
                        models: Vec::new(),
                        has_bands: false,
                    },
                )
            })
            .collect(),
    }
}

fn enqueue_one(ctx: &Ctx, st: &mut State) -> Result<String, VivariumError> {
    enqueue(
        ctx,
        st,
        "board",
        "n1",
        100,
        "plan",
        60,
        "board/n1.out",
        "plans/n1.json",
        None,
    )
}

#[test]
fn planner_jobs_go_to_pump_planner_and_a_role_named_planner_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(planner_routing(&[
        ("planner", "seat-pool"),
        ("pump-planner", "pump-pool"),
    ]));
    let mut st = State::default();
    let id = enqueue_one(&ctx, &mut st).unwrap();
    let job = &st.jobs()[&id];
    assert_eq!(job.role, "pump-planner");
    assert_eq!(job.pool_candidates, ["pump-pool"]);

    let ctx = ctx.with_routing(planner_routing(&[("planner", "seat-pool")]));
    let mut st = State::default();
    let error = enqueue_one(&ctx, &mut st).unwrap_err().to_string();
    assert!(error.contains("pump-planner"), "{error}");
    assert!(error.contains("vivi config migrate"), "{error}");
    assert!(st.jobs().is_empty());
}
