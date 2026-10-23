use super::{SliceMergeResult, classify};
use crate::pump::jobs::rung_result::{EnvKind, env_failure};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

const SHA: &str = "2222222222222222222222222222222222222222";
const CAND: &str = "1111111111111111111111111111111111111111";

fn job(status: JobStatus, state: &str, receipt: Option<&str>) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(
        "slice-merge-x",
        "slice-merge",
        "merge",
        "s",
        "b",
    ));
    job.status = status;
    job.outcome = receipt.map(|receipt| JobOutcome {
        state: state.to_owned(),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 0,
    });
    job
}

fn finished(receipt: &str) -> Option<SliceMergeResult> {
    classify(&job(JobStatus::Finished, "finished", Some(receipt)))
}

#[test]
fn pending_and_dispatched_are_not_read() {
    assert_eq!(classify(&job(JobStatus::Pending, "finished", None)), None);
    assert_eq!(
        classify(&job(JobStatus::Dispatched, "finished", None)),
        None
    );
}

#[test]
fn timeout_and_lost_are_unclear() {
    for status in [JobStatus::Timeout, JobStatus::Lost] {
        assert_eq!(
            classify(&job(status, "finished", None)),
            Some(SliceMergeResult::Unclear)
        );
    }
}

#[test]
fn a_finished_job_with_no_outcome_is_unclear() {
    assert_eq!(
        classify(&job(JobStatus::Finished, "finished", None)),
        Some(SliceMergeResult::Unclear)
    );
}

#[test]
fn an_outcome_state_of_timeout_or_lost_is_unclear() {
    for state in ["timeout", "lost"] {
        let receipt = "agent exit 1 in 3s\nCONFLICT (content): Merge conflict in a.rs";
        assert_eq!(
            classify(&job(JobStatus::Finished, state, Some(receipt))),
            Some(SliceMergeResult::Unclear)
        );
    }
}

#[test]
fn conflicts_list_sorted_deduplicated_paths() {
    let receipt = "Auto-merging src/b.rs\n\
CONFLICT (content): Merge conflict in src/b.rs\n\
CONFLICT (content): Merge conflict in src/a.rs\n\
CONFLICT (content): Merge conflict in src/b.rs\n\
Automatic merge failed\nagent exit 1 in 2s";
    assert_eq!(
        finished(receipt),
        Some(SliceMergeResult::Conflict(vec![
            String::from("src/a.rs"),
            String::from("src/b.rs"),
        ]))
    );
}

#[test]
fn a_failed_command_without_conflict_is_red_with_the_tail() {
    let receipt = "--- stderr tail\ntest result: FAILED. 1 failed\nagent exit 101 in 40s";
    let Some(SliceMergeResult::Red(tail)) = finished(receipt) else {
        panic!("expected red");
    };
    assert!(tail.contains("test result: FAILED"), "{tail}");
}

#[test]
fn a_publish_line_after_a_clean_exit_is_merged() {
    let receipt =
        format!("agent exit 0 in 9s\nrunner publish: pushed refs/heads/vivi/need/n1 {CAND} {SHA}");
    assert_eq!(
        finished(&receipt),
        Some(SliceMergeResult::Merged(SHA.to_owned()))
    );
}

#[test]
fn a_clean_exit_with_no_publish_line_is_unclear() {
    assert_eq!(
        finished("agent exit 0 in 9s"),
        Some(SliceMergeResult::Unclear)
    );
}

#[test]
fn a_refused_publish_is_unclear() {
    let receipt = format!(
        "agent exit 0 in 9s\nrunner publish: push_refused refs/heads/vivi/need/n1 {CAND} x"
    );
    assert_eq!(finished(&receipt), Some(SliceMergeResult::Unclear));
}

const NO_TSC: &str = "agent exit 127 in 1s\n\n--- stderr tail\nbash: tsc: command not found\n";

#[test]
fn a_missing_tool_is_an_environment_failure_not_red() {
    let Some(SliceMergeResult::Env(env)) = finished(NO_TSC) else {
        panic!("expected an environment failure");
    };
    assert_eq!(env.kind, EnvKind::ToolMissing);
    assert_eq!(env.tool.as_deref(), Some("tsc"));
}

#[test]
fn a_conflict_receipt_stays_a_conflict_even_with_a_shell_marker() {
    let receipt = format!("CONFLICT (content): Merge conflict in a.rs\n{NO_TSC}");
    assert_eq!(
        finished(&receipt),
        Some(SliceMergeResult::Conflict(vec![String::from("a.rs")]))
    );
}

#[test]
fn a_setup_failure_outcome_on_a_lost_job_is_an_environment_failure() {
    let mut lost = job(JobStatus::Lost, "finished", Some(""));
    if let Some(outcome) = lost.outcome.as_mut() {
        outcome.detail = String::from("checkout_failed no such ref");
    }
    let expected = env_failure(&lost).expect("env failure");
    assert_eq!(expected.kind, EnvKind::HostSetup);
    assert_eq!(classify(&lost), Some(SliceMergeResult::Env(expected)));
}

#[test]
fn a_setup_failure_with_no_agent_exit_is_an_environment_failure() {
    let mut done = job(JobStatus::Finished, "finished", Some(""));
    if let Some(outcome) = done.outcome.as_mut() {
        outcome.detail = String::from("fetch_failed network down");
    }
    assert!(matches!(
        classify(&done),
        Some(SliceMergeResult::Env(env)) if env.kind == EnvKind::HostSetup
    ));
}
