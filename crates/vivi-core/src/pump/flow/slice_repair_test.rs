//! Tests for [`super::Repair`]: what a failed job gives the next attempt.

use super::Repair;
use crate::pump::jobs::{JobOutcome, JobSpec, PoolJob};

const HAND: &str = "2222222222222222222222222222222222222222";

fn failed(receipt: &str, payload: serde_json::Value) -> PoolJob {
    let mut spec = JobSpec::new("merge-1", "slice-merge", "merge", "s", "b");
    spec.payload = payload;
    let mut job = PoolJob::from(spec);
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 1,
    });
    job
}

#[test]
fn a_red_merge_starts_the_repair_from_the_failed_commit_with_the_failure() {
    let job = failed(
        "--- stderr tail\ntest result: FAILED. 1 failed\nagent exit 101 in 40s",
        serde_json::json!({"hand_sha": HAND}),
    );
    let repair = Repair::of(&job, 1);
    assert_eq!(repair.from.as_deref(), Some(HAND));
    assert!(repair.failure.contains("test result: FAILED"), "{repair:?}");
    let section = repair.section();
    assert!(section.starts_with("EARLIER ATTEMPT:\nAttempt 1 of this slice failed."));
    assert!(section.contains(HAND), "{section}");
    assert!(section.contains("do not start the work over"), "{section}");
}

#[test]
fn a_conflict_starts_the_repair_from_the_need_tip_with_the_conflicting_paths() {
    let job = failed(
        "CONFLICT (content): Merge conflict in src/a.rs\nagent exit 1 in 2s",
        serde_json::json!({"hand_sha": HAND}),
    );
    let repair = Repair::of(&job, 1);
    assert_eq!(repair.from, None);
    assert!(repair.failure.contains("src/a.rs"), "{repair:?}");
    assert!(repair.section().contains("starts from the need-branch tip"));
}

#[test]
fn a_hand_job_that_left_no_commit_starts_from_the_need_tip() {
    let mut job = failed("agent exit 1 in 3s", serde_json::json!({}));
    if let Some(outcome) = job.outcome.as_mut() {
        outcome.detail = String::from("hand failed: the brief's premise is wrong");
    }
    let repair = Repair::of(&job, 2);
    assert_eq!(repair.from, None);
    assert!(repair.failure.contains("premise is wrong"), "{repair:?}");
    assert!(repair.section().contains("Attempt 2 of this slice failed."));
}
