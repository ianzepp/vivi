use super::{MergeMainResult, classify, spec};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, PoolJob};
use crate::routing::RoutedRole;

const TIP: &str = "1111111111111111111111111111111111111111";
const MERGED: &str = "2222222222222222222222222222222222222222";
const NEED_TIP: &str = "3333333333333333333333333333333333333333";
const NEWER_TIP: &str = "4444444444444444444444444444444444444444";

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
    let mut job = PoolJob::from(spec(&ctx, "default", "n1", NEED_TIP, TIP, "step1").unwrap());
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

fn classify_receipt(receipt: &str) -> Option<MergeMainResult> {
    classify(&job_with(JobStatus::Finished, finished(receipt)))
}

#[test]
fn spec_merges_the_need_tip_and_publishes_main_fast_forward_only() {
    let (_dir, ctx) = merge_ctx();

    let job = spec(&ctx, "default", "n1", NEED_TIP, TIP, "step1").unwrap();

    assert_eq!(job.kind, "merge");
    assert_eq!(job.role, "merge");
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert_eq!(job.with, vec![String::from(NEED_TIP)]);
    assert_eq!(job.publish_ref.as_deref(), Some("refs/heads/main"));
    assert!(job.publish_expect.is_none());
    assert_eq!(job.publish_contains.as_deref(), Some(NEED_TIP));
    assert!(!job.allow_default_branch);
    assert!(job.model.is_none());
    let command = job.command.unwrap();
    assert!(
        command.starts_with("git merge --no-ff --no-edit -m \""),
        "{command}"
    );
    assert!(command.ends_with(&format!(" {NEED_TIP}")), "{command}");
}

#[test]
fn the_job_runs_on_a_scratch_branch_so_pool_dispatch_accepts_it() {
    let (_dir, ctx) = merge_ctx();

    let job = spec(&ctx, "default", "n1", NEED_TIP, TIP, "step1").unwrap();

    let branch = job.branch.unwrap();
    let rest = branch.strip_prefix("pump/merge-main/").unwrap();
    assert!(rest.starts_with("default-merge-main-step1-"), "{branch}");
    assert!(!rest.is_empty() && !rest.contains('/'), "{branch}");
    assert_ne!(branch, "main");
}

#[test]
fn the_job_id_differs_per_main_tip_so_a_cas_retry_enqueues() {
    let (_dir, ctx) = merge_ctx();

    let first = spec(&ctx, "default", "n1", NEED_TIP, TIP, "step1").unwrap();
    let retry = spec(&ctx, "default", "n1", NEED_TIP, NEWER_TIP, "step1").unwrap();
    let again = spec(&ctx, "default", "n1", NEED_TIP, TIP, "step1").unwrap();

    assert_ne!(first.id, retry.id);
    assert_ne!(first.branch, retry.branch);
    assert_eq!(first.id, again.id);
}

#[test]
fn the_merge_message_carries_no_ids_or_branch_names() {
    let (_dir, ctx) = merge_ctx();
    let job = spec(&ctx, "default", "n1", NEED_TIP, TIP, "step1").unwrap();
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

    assert!(spec(&ctx, "default", "n1", NEED_TIP, TIP, "step1").is_none());
}

#[test]
fn a_publish_receipt_is_merged_with_the_new_main_sha() {
    let receipt =
        format!("agent exit 0 in 1s\nrunner publish: pushed refs/heads/main {MERGED} {MERGED}");

    assert_eq!(
        classify_receipt(&receipt),
        Some(MergeMainResult::Merged(String::from(MERGED)))
    );
}

#[test]
fn a_git_conflict_report_is_a_conflict() {
    let result = classify_receipt("agent exit 1 in 1s\nCONFLICT (content): Merge conflict in a.rs");

    assert_eq!(result, Some(MergeMainResult::Conflict));
}

#[test]
fn a_failed_git_command_without_a_conflict_is_failed_not_a_conflict() {
    let result = classify_receipt("agent exit 1 in 1s\nmerge: 3333 - not something we can merge");

    assert_eq!(result, Some(MergeMainResult::Failed));
}

#[test]
fn a_moved_main_is_a_conflict() {
    // The runner leaves the remote sha empty when ancestry fails.
    let not_ancestor =
        format!("agent exit 0 in 1s\nrunner publish: not_ancestor refs/heads/main {MERGED} ");
    let refused =
        format!("agent exit 0 in 1s\nrunner publish: push_refused refs/heads/main {MERGED} ");

    assert_eq!(
        classify_receipt(&not_ancestor),
        Some(MergeMainResult::Conflict)
    );
    assert_eq!(classify_receipt(&refused), Some(MergeMainResult::Conflict));
}

#[test]
fn a_candidate_mismatch_or_other_refused_publish_is_failed() {
    for state in [
        "candidate_mismatch",
        "required_source_missing",
        "candidate_missing",
        "verify_failed",
    ] {
        let receipt =
            format!("agent exit 0 in 1s\nrunner publish: {state} refs/heads/main {MERGED} ");

        assert_eq!(
            classify_receipt(&receipt),
            Some(MergeMainResult::Failed),
            "{state}"
        );
    }
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
    assert_eq!(
        classify_receipt("agent exit 0 in 1s"),
        Some(MergeMainResult::Failed)
    );
}

#[test]
fn a_job_still_running_is_not_classified() {
    assert_eq!(classify(&job_with(JobStatus::Dispatched, None)), None);
}
