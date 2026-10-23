use super::{MergeMainResult, classify, spec};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, PoolJob};
use crate::routing::RoutedRole;

const TIP: &str = "1111111111111111111111111111111111111111";
const MERGED: &str = "2222222222222222222222222222222222222222";

fn merge_ctx() -> (tempfile::TempDir, Ctx) {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    (dir, ctx.with_routing(routing))
}

fn job_with(status: JobStatus, outcome: Option<JobOutcome>) -> PoolJob {
    let (_dir, ctx) = merge_ctx();
    let mut job = PoolJob::from(spec(&ctx, "default", "n1", "vivi/need/n1", TIP, "step1").unwrap());
    job.status = status;
    job.outcome = outcome;
    job
}

fn finished(receipt: &str) -> Option<JobOutcome> {
    Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 0,
    })
}

#[test]
fn spec_merges_the_need_branch_and_publishes_main_by_compare_and_swap() {
    let (_dir, ctx) = merge_ctx();

    let job = spec(&ctx, "default", "n1", "vivi/need/n1", TIP, "step1").unwrap();

    assert_eq!(job.kind, "merge");
    assert_eq!(job.role, "merge");
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert_eq!(job.with, vec![String::from("vivi/need/n1")]);
    assert_eq!(job.publish_ref.as_deref(), Some("refs/heads/main"));
    assert_eq!(job.publish_expect.as_deref(), Some(TIP));
    assert!(job.allow_default_branch);
    assert!(job.model.is_none());
    let command = job.command.unwrap();
    assert!(
        command.starts_with("git merge --no-ff --no-edit -m \""),
        "{command}"
    );
    assert!(command.ends_with(" vivi/need/n1"), "{command}");
}

#[test]
fn the_merge_message_carries_no_ids_or_branch_names() {
    let (_dir, ctx) = merge_ctx();
    let job = spec(&ctx, "default", "n1", "vivi/need/n1", TIP, "step1").unwrap();
    let command = job.command.unwrap();
    let message = command
        .split_once("-m \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(message, _)| message)
        .unwrap();

    assert!(!message.contains("n1"), "{message}");
    assert!(!message.contains("step1"), "{message}");
    assert!(!message.contains('/'), "{message}");
    assert!(!message.contains("need"), "{message}");
}

#[test]
fn no_merge_role_gives_no_spec() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());

    assert!(spec(&ctx, "default", "n1", "vivi/need/n1", TIP, "step1").is_none());
}

#[test]
fn a_publish_receipt_is_merged_with_the_new_main_sha() {
    let receipt =
        format!("agent exit 0 in 1s\nrunner publish: pushed refs/heads/main {MERGED} {MERGED}");

    let result = classify(&job_with(JobStatus::Finished, finished(&receipt)));

    assert_eq!(result, Some(MergeMainResult::Merged(String::from(MERGED))));
}

#[test]
fn a_failed_merge_command_is_a_conflict() {
    let result = classify(&job_with(
        JobStatus::Finished,
        finished("agent exit 1 in 1s\nCONFLICT (content)"),
    ));

    assert_eq!(result, Some(MergeMainResult::Conflict));
}

#[test]
fn a_refused_publish_is_a_conflict() {
    let receipt =
        format!("agent exit 0 in 1s\nrunner publish: stale refs/heads/main {MERGED} {TIP}");

    let result = classify(&job_with(JobStatus::Finished, finished(&receipt)));

    assert_eq!(result, Some(MergeMainResult::Conflict));
}

#[test]
fn a_timeout_or_lost_job_is_failed() {
    assert_eq!(
        classify(&job_with(JobStatus::Timeout, None)),
        Some(MergeMainResult::Failed)
    );
    assert_eq!(
        classify(&job_with(JobStatus::Lost, None)),
        Some(MergeMainResult::Failed)
    );
}

#[test]
fn a_clean_exit_without_a_publish_receipt_is_failed() {
    let result = classify(&job_with(
        JobStatus::Finished,
        finished("agent exit 0 in 1s"),
    ));

    assert_eq!(result, Some(MergeMainResult::Failed));
}

#[test]
fn a_job_still_running_is_not_classified() {
    assert_eq!(classify(&job_with(JobStatus::Dispatched, None)), None);
}
