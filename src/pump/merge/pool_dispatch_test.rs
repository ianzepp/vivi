use std::collections::BTreeSet;

use super::{
    MergeReceipt, SourceResult, parse_receipt, published_integration_tip, runner_result_matches,
    valid_sha, valid_sources,
};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, PoolJob};

#[test]
fn merge_receipt_parses_the_structured_worker_result() {
    let text = "pool result j1: finished\nagent exit 0 in 2s\nPUMP_MERGE_RESULT={\"version\":1,\"target\":\"default\",\"tip\":\"0123456789012345678901234567890123456789\",\"sources\":[{\"kind\":\"unit\",\"id\":\"u1\",\"status\":\"merged\",\"outside\":[],\"secs\":2}]}";
    let receipt = parse_receipt(text).unwrap();
    assert_eq!(receipt.version, 1);
    assert_eq!(receipt.target, "default");
    assert_eq!(receipt.sources[0].id, "u1");
}

#[test]
fn merge_receipt_rejects_missing_or_duplicated_sources() {
    let expected_units = BTreeSet::from([String::from("u1")]);
    let expected_jobs = BTreeSet::from([String::from("role-job")]);
    let unit = SourceResult {
        kind: String::from("unit"),
        id: String::from("u1"),
        status: String::from("merged"),
        why: String::new(),
        detail: String::new(),
        outside: Vec::new(),
        secs: None,
    };
    let role_job = SourceResult {
        kind: String::from("job"),
        id: String::from("role-job"),
        status: String::from("undecided"),
        why: String::from("lane-race"),
        detail: String::from("branch is clean against refreshed tip"),
        outside: Vec::new(),
        secs: None,
    };
    assert!(valid_sources(
        &[unit.clone(), role_job.clone()],
        &expected_units,
        &expected_jobs,
    ));
    assert!(!valid_sources(
        &[unit.clone()],
        &expected_units,
        &expected_jobs
    ));
    assert!(!valid_sources(
        &[unit.clone(), unit, role_job],
        &expected_units,
        &expected_jobs,
    ));
}

#[test]
fn pool_output_must_name_the_job_branch_and_candidate_sha() {
    let mut job = PoolJob::from(JobSpec::new("merge-1", "merge", "merge", "s", "b"));
    job.branch = Some(String::from("pump/merge/default/merge-default-1"));
    let outcome = JobOutcome {
        state: String::from("finished"),
        detail: String::from("pushed pump/merge/default/merge-default-1 0123456789"),
        receipt: String::new(),
        at: 1,
    };
    assert!(runner_result_matches(
        &outcome,
        &job,
        "0123456789012345678901234567890123456789"
    ));
    assert!(!runner_result_matches(
        &JobOutcome {
            detail: String::from("committed other 0123456789"),
            ..outcome.clone()
        },
        &job,
        "0123456789012345678901234567890123456789"
    ));
    assert!(runner_result_matches(
        &JobOutcome {
            detail: String::from("no_changes"),
            ..outcome
        },
        &job,
        "0123456789012345678901234567890123456789"
    ));
    assert!(valid_sha("0123456789012345678901234567890123456789"));
    assert!(!valid_sha("not-a-commit"));
}

#[test]
fn merge_result_requires_remote_integration_ancestry() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (ctx, host) = test_ctx(dir.path(), config);
    let candidate = "0123456789012345678901234567890123456789";
    let remote_tip = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    host.reply("fetch --quiet origin refs/heads/next", 0, "");
    host.reply(
        "rev-parse --verify --quiet FETCH_HEAD",
        0,
        &format!("{remote_tip}\n"),
    );
    host.reply(
        &format!("merge-base --is-ancestor {candidate} {remote_tip}"),
        0,
        "",
    );
    let mut job = PoolJob::from(JobSpec::new("merge-1", "merge", "merge", "s", "b"));
    job.target = Some(String::from("default"));

    assert_eq!(
        published_integration_tip(&ctx, &job, candidate).as_deref(),
        Some(remote_tip)
    );
    assert!(host.commands().iter().any(|command| {
        command.contains("merge-base --is-ancestor") && command.contains(remote_tip)
    }));
}

#[test]
fn merge_receipt_requires_the_versioned_fields() {
    let value = serde_json::json!({
        "version": 1,
        "target": "default",
        "tip": "0123456789012345678901234567890123456789",
        "sources": []
    });
    let parsed: MergeReceipt = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.version, 1);
    assert!(parsed.sources.is_empty());
}
