//! Tests for the shared budget-retry helpers.

use super::{
    PAYLOAD_KEY, is_budget_kill, is_thinking, mark_retry, raised_budget, rerun_spec, spent,
};
use crate::pool::dispatch::BUDGET_RANGE;
use crate::pump::envelope::roles::RoleKind;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

/// A durable job in `status` with an empty payload and no outcome.
fn job(status: JobStatus) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new("j", "step", "hand", "subject", "body"));
    job.status = status;
    job
}

/// A finished job whose collected receipt is `receipt`.
fn finished(receipt: &str) -> PoolJob {
    let mut job = job(JobStatus::Finished);
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 7,
    });
    job
}

/// A job in `status` whose collected receipt claims a timeout kill.
fn killed(status: JobStatus) -> PoolJob {
    let mut job = job(status);
    job.outcome = Some(JobOutcome {
        state: "lost".to_owned(),
        detail: String::new(),
        receipt: "agent exit 124 after 600s\n".to_owned(),
        at: 7,
    });
    job
}

/// A pool summary head reporting the timeout exit.
const KILL_SUMMARY: &str = "unit a1\nagent exit 124 after 600s\npool: local";

#[test]
fn budget_raise_respects_pool_limit() {
    let maximum = *BUDGET_RANGE.end();
    // Ordinary doubling inside the range.
    assert_eq!(raised_budget(600), Some(1_200));
    // Doubling past the pool limit lands exactly on the maximum.
    assert_eq!(raised_budget(maximum - 1), Some(maximum));
    // A budget already at the ceiling has no strict raise left.
    assert_eq!(raised_budget(maximum), None);
    // Zero is not a usable budget.
    assert_eq!(raised_budget(0), None);
    // Above the ceiling the saved budget is unusable, however doubled.
    assert_eq!(raised_budget(maximum + 1), None);
    // Doubling saturates and the input is out of range anyway.
    assert_eq!(raised_budget(u64::MAX), None);
}

#[test]
fn budget_kill_is_machine_evidence_only() {
    // Representation one: the pool's terminal timeout status needs no text.
    assert!(is_budget_kill(&job(JobStatus::Timeout)));
    // Representation two: finished with agent exit 124 in the summary head.
    assert!(is_budget_kill(&finished(KILL_SUMMARY)));
    // A summary kill survives a one-newline section boundary.
    let tight = "agent exit 124 after 600s\n--- stderr tail\nnoise";
    assert!(is_budget_kill(&finished(tight)));
    // A legacy receipt counts only before the hand's own text: a clean
    // summary exit under the old boundary is not a kill, and 124 written
    // after it is never read.
    let clean_legacy = "unit a1\nagent exit 0 in 8s\nhand's final message:\nbuilt";
    assert!(!is_budget_kill(&finished(clean_legacy)));
    let killed_legacy =
        format!("{KILL_SUMMARY}\nhand's final message:\nI saw agent exit 124 in a log line.");
    assert!(is_budget_kill(&finished(&killed_legacy)));
}

#[test]
fn budget_kill_never_reads_section_text_or_other_statuses() {
    // A successful reply whose receipt section merely mentions exit 124.
    let ok_prose = "unit a1\nagent exit 0 in 8s\n\n--- receipt\nchild command agent exit 124";
    assert!(!is_budget_kill(&finished(ok_prose)));
    // The same fixture with only one newline before the first section.
    let tight_prose = "unit a1\nagent exit 0 in 8s\n--- receipt\nchild command agent exit 124";
    assert!(!is_budget_kill(&finished(tight_prose)));
    // A summary with no exit line at all, followed by a quoted exit code in
    // the receipt section, is not a kill.
    let absent_summary = "unit a1 done\n--- receipt\nagent exit 124";
    assert!(!is_budget_kill(&finished(absent_summary)));
    // Child-command output in the stderr and runner-log tails never counts.
    let tails = "unit a1\nagent exit 0 in 8s\n\n--- stderr tail\n\
        `cargo test` exited: agent exit 124\n\n--- runner log tail\n\
        child command output: agent exit 124 after 30s";
    assert!(!is_budget_kill(&finished(tails)));
    // Numeric codes other than exactly 124 are not timeout kills, even when
    // only the summary would be read.
    assert!(!is_budget_kill(&finished("agent exit 1 after 600s\n")));
    assert!(!is_budget_kill(&finished("agent exit 1240 after 600s\n")));
    // Pending, dispatched and lost jobs are never finished budget kills.
    assert!(!is_budget_kill(&killed(JobStatus::Pending)));
    assert!(!is_budget_kill(&killed(JobStatus::Dispatched)));
    assert!(!is_budget_kill(&killed(JobStatus::Lost)));
}

#[test]
fn is_thinking_covers_exactly_the_seven_kinds() {
    for role in [
        RoleKind::Researcher,
        RoleKind::Planner,
        RoleKind::Admitter,
        RoleKind::Slicer,
        RoleKind::Auditor,
        RoleKind::Judge,
        RoleKind::Cto,
    ] {
        assert!(is_thinking(role));
    }
    for role in [
        RoleKind::Merge,
        RoleKind::Hand,
        RoleKind::Repairer,
        RoleKind::HeadCeo,
    ] {
        assert!(!is_thinking(role));
    }
}

#[test]
fn retry_marker_defaults_to_unspent() {
    // The payload key missing means the retry is unspent, a non-boolean
    // value means false, and marking is durable.
    let mut retry = finished(KILL_SUMMARY);
    assert!(!spent(&retry));
    retry.payload = serde_json::json!({ PAYLOAD_KEY: "yes" });
    assert!(!spent(&retry));
    retry.payload = serde_json::json!({});
    mark_retry(&mut retry.payload);
    assert_eq!(
        retry.payload.get(PAYLOAD_KEY),
        Some(&serde_json::Value::Bool(true))
    );
    assert!(spent(&retry));
    // A null payload grows the marker; another shape is left untouched.
    let mut null_payload = job(JobStatus::Finished);
    mark_retry(&mut null_payload.payload);
    assert!(spent(&null_payload));
    let mut scalar = job(JobStatus::Finished);
    scalar.payload = serde_json::Value::from(7);
    mark_retry(&mut scalar.payload);
    assert_eq!(scalar.payload, serde_json::Value::from(7));
}

#[test]
fn rerun_spec_copies_every_execution_field() {
    let mut spec = JobSpec::new("j", "slice", "slicer", "subject", "body");
    spec.pool_candidates = vec!["fast".into(), "slow".into()];
    spec.local_only = true;
    spec.pool_exclude = vec!["shared".into()];
    spec.target = Some("repo-a".into());
    spec.extra_targets = vec!["repo-b".into()];
    spec.from_ref = Some("origin/main".into());
    spec.with = vec!["refs/heads/x".into()];
    spec.model = Some("m".into());
    spec.command = Some("make test".into());
    spec.builtin_role = Some("slicer".into());
    spec.branch = Some("need/1".into());
    spec.allow_default_branch = true;
    spec.publish_ref = Some("refs/heads/p".into());
    spec.publish_expect = Some("abc".into());
    spec.publish_contains = Some("def".into());
    spec.delete_ref = Some("refs/heads/old".into());
    spec.delete_expect = Some("bcd".into());
    spec.budget = 600;
    spec.read_only = true;
    spec.units = vec!["u1".into()];
    spec.source_jobs = vec!["sj".into()];
    spec.payload = serde_json::json!({ "step_task": "t1", (PAYLOAD_KEY): true });
    spec.task = Some("task-9".into());

    let mut owned = PoolJob::from(spec.clone());
    owned.queued_at = 5;
    owned.status = JobStatus::Dispatched;
    owned.pool = Some("fast".into());
    owned.dispatched_at = Some(11);
    owned.outcome = Some(JobOutcome {
        state: "lost".to_owned(),
        detail: "detail".to_owned(),
        receipt: "agent exit 124 after 600s\n".to_owned(),
        at: 12,
    });
    owned.applied_at = Some(13);
    owned.dispatch_refusals = 2;
    owned.held = Some("refused".into());

    let mut expected = spec;
    expected.id = "j2".to_owned();
    assert_eq!(rerun_spec(&owned, "j2"), expected);
}
