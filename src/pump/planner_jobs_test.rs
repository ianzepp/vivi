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
