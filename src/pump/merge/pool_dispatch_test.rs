use std::collections::{BTreeMap, BTreeSet};

use super::{
    MERGE_TIMEOUT_RULES, MergeReceipt, SourceResult, apply_source, parse_receipt,
    published_integration_tip, record_shared_timeout_exception, timeout_status_line, valid_sha,
    valid_sources,
};
use crate::pump::config::{Generated, PumpConfig};
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};
use crate::pump::merge::dispatch_pool;
use crate::pump::state::{State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

#[test]
fn merge_receipt_parses_the_structured_worker_result() {
    let text = "pool result j1: finished\nagent exit 0 in 2s\nPUMP_MERGE_RESULT={\"version\":1,\"target\":\"default\",\"tip\":\"0123456789012345678901234567890123456789\",\"sources\":[{\"kind\":\"unit\",\"id\":\"u1\",\"status\":\"merged\",\"outside\":[],\"secs\":2}]}";
    let receipt = parse_receipt(text).unwrap();
    assert_eq!(receipt.version, 1);
    assert_eq!(receipt.target, "default");
    assert_eq!(receipt.sources[0].id, "u1");
}

#[test]
fn missing_or_unusable_merge_route_raises_exception_without_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (base, host) = test_ctx(dir.path(), PumpConfig::default());
    let unusable = [
        RoleRouting::default(),
        RoleRouting {
            roles: vec![merge_role("paused", Some("local"))],
            pools: BTreeMap::from([(String::from("local"), local_pool())]),
        },
        RoleRouting {
            roles: vec![merge_role("active", Some("missing"))],
            pools: BTreeMap::new(),
        },
    ];
    for routing in unusable {
        let ctx = base.clone().with_routing(routing);
        let mut state = State::default();

        assert!(dispatch_pool(&ctx, &mut state).unwrap().is_empty());
        assert!(state.jobs.is_empty());
        assert!(
            host.logs()
                .iter()
                .any(|line| line.contains("EXCEPTION merge-route-missing"))
        );
    }
}

fn merge_role(status: &str, pool: Option<&str>) -> RoutedRole {
    RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: status.to_owned(),
        pool: pool.map(str::to_owned),
        read_only: Some(false),
        model: None,
        models: Vec::new(),
    }
}

fn local_pool() -> RoutedPool {
    RoutedPool {
        slots: 1,
        provider: String::from("local"),
        fallback: None,
        caps: BTreeMap::new(),
        models: Vec::new(),
        has_bands: false,
    }
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
fn an_exhausted_shared_timeout_must_leave_every_source_undecided() {
    let expected_units = BTreeSet::from([String::from("u1"), String::from("u2")]);
    let undecided = |id: &str| SourceResult {
        kind: String::from("unit"),
        id: id.to_owned(),
        status: String::from("undecided"),
        why: String::from("shared-check-timeout"),
        detail: String::from("cargo check timed out after 300s; retry timed out"),
        outside: Vec::new(),
        secs: None,
    };
    let mut merged = undecided("u2");
    merged.status = String::from("merged");
    merged.why.clear();
    merged.detail.clear();

    assert!(valid_sources(
        &[undecided("u1"), undecided("u2")],
        &expected_units,
        &BTreeSet::new()
    ));
    assert!(!valid_sources(
        &[undecided("u1"), merged],
        &expected_units,
        &BTreeSet::new()
    ));
}

#[test]
fn runner_publish_receipt_names_ref_candidate_and_remote_sha() {
    let candidate = "0123456789012345678901234567890123456789";
    let remote_tip = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
    let outcome = JobOutcome {
        state: String::from("finished"),
        detail: String::from("no_changes"),
        receipt: format!("runner publish: pushed refs/heads/next {candidate} {remote_tip}"),
        at: 1,
    };
    let publish = outcome.runner_publish().unwrap();
    assert!(publish.succeeded());
    assert_eq!(publish.reference, "refs/heads/next");
    assert_eq!(publish.candidate, candidate);
    assert_eq!(publish.remote_sha, remote_tip);
    assert!(valid_sha("0123456789012345678901234567890123456789"));
    assert!(!valid_sha("not-a-commit"));
}

#[test]
fn lane_race_and_generated_publish_conflicts_retry_solo_without_charging_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.merge.generated.push(Generated {
        paths: vec![String::from("generated/counter.rs")],
        regenerate: String::from("./regenerate-counter"),
        drift: Vec::new(),
    });
    let (ctx, _) = test_ctx(dir.path(), config);
    let mut job = PoolJob::from(JobSpec::new("merge-1", "merge", "merge", "s", "b"));
    job.units = vec![String::from("u1")];
    for (why, detail) in [
        (
            "lane-race",
            "source was clean against the refreshed integration tip",
        ),
        (
            "generated-publish-conflict",
            "publish conflicted only in configured generated/counter.rs after green checks",
        ),
    ] {
        let mut st = State::default();
        st.units.insert(
            String::from("u1"),
            Unit {
                status: Status::Merging,
                attempts: 2,
                ..Unit::default()
            },
        );
        let source = SourceResult {
            kind: String::from("unit"),
            id: String::from("u1"),
            status: String::from("undecided"),
            why: String::from(why),
            detail: String::from(detail),
            outside: Vec::new(),
            secs: Some(1),
        };

        apply_source(
            &ctx,
            &mut st,
            &job,
            &source,
            "0123456789012345678901234567890123456789",
        );

        let unit = &st.units["u1"];
        assert_eq!(unit.status, Status::Delivered, "{why}");
        assert!(unit.solo, "{why}");
        assert_eq!(unit.attempts, 2, "{why}");
        assert_eq!(st.merges, 0, "{why}");
    }
    assert_eq!(ctx.cfg.merge.generated_paths(), ["generated/counter.rs"]);
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
    job.publish_ref = Some(String::from("refs/heads/next"));
    let outcome = JobOutcome {
        state: String::from("finished"),
        detail: String::from("no_changes"),
        receipt: format!("runner publish: pushed refs/heads/next {candidate} {remote_tip}"),
        at: 1,
    };

    assert_eq!(
        published_integration_tip(&ctx, &job, &outcome, candidate).as_deref(),
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

#[test]
fn worker_recovery_receipt_preserves_check_identity_and_first_failure() {
    let receipt = parse_receipt(
        "PUMP_MERGE_RESULT={\"version\":1,\"target\":\"default\",\"tip\":\"0123456789012345678901234567890123456789\",\"shared_check_timeouts\":0,\"shared_check_timeout_retries\":0,\"shared_check_recoveries\":[{\"name\":\"stage-4b\",\"command\":\"cargo test --workspace\",\"detail\":\"first run was transiently locked\"}],\"sources\":[]}",
    )
    .unwrap();
    let recovery = &receipt.shared_check_recoveries.unwrap()[0];
    assert_eq!(recovery.name, "stage-4b");
    assert_eq!(recovery.command, "cargo test --workspace");
    assert_eq!(recovery.detail, "first run was transiently locked");
}

#[test]
fn merge_timeout_brief_specifies_retry_bisect_and_metric_contract() {
    assert!(MERGE_TIMEOUT_RULES.contains("shared-check-timeout"));
    assert!(MERGE_TIMEOUT_RULES.contains("bisect-check-timeout"));
    assert!(MERGE_TIMEOUT_RULES.contains("shared_check_timeouts"));
    assert!(MERGE_TIMEOUT_RULES.contains("shared_check_timeout_retries"));
    assert!(MERGE_TIMEOUT_RULES.contains("shared_check_recoveries"));
    assert!(MERGE_TIMEOUT_RULES.contains("same tree"));
}

#[test]
fn status_sums_timeout_metrics_from_terminal_merge_worker_receipts() {
    let mut state = State::default();
    for (id, timeouts, retries) in [("merge-1", 2, 1), ("merge-2", 1, 1)] {
        let mut job = PoolJob::from(JobSpec::new(id, "merge", "merge", "s", "b"));
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: String::from("finished"),
            detail: String::from("done"),
            receipt: format!(
                "PUMP_MERGE_RESULT={{\"version\":1,\"target\":\"default\",\"tip\":\"0123456789012345678901234567890123456789\",\"shared_check_timeouts\":{timeouts},\"shared_check_timeout_retries\":{retries},\"shared_check_recoveries\":[],\"sources\":[]}}"
            ),
            at: 1,
        });
        state.jobs.insert(String::from(id), job);
    }

    assert_eq!(
        timeout_status_line(&state),
        "shared-check timeouts 3, timeout retries 2, recovered checks 0"
    );
}

#[test]
fn status_does_not_turn_missing_timeout_metrics_into_zeroes() {
    let mut state = State::default();
    let mut job = PoolJob::from(JobSpec::new("merge-old", "merge", "merge", "s", "b"));
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("done"),
        receipt: String::from(
            "PUMP_MERGE_RESULT={\"version\":1,\"target\":\"default\",\"tip\":\"0123456789012345678901234567890123456789\",\"sources\":[]}",
        ),
        at: 1,
    });
    state.jobs.insert(String::from("merge-old"), job);

    assert!(timeout_status_line(&state).contains("metrics incomplete"));
    assert!(timeout_status_line(&State::default()).contains("unavailable"));
}

#[test]
fn status_counts_recovered_shared_checks_from_worker_receipts() {
    let mut state = State::default();
    let mut job = PoolJob::from(JobSpec::new("merge-recovered", "merge", "merge", "s", "b"));
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("done"),
        receipt: String::from(
            "PUMP_MERGE_RESULT={\"version\":1,\"target\":\"default\",\"tip\":\"0123456789012345678901234567890123456789\",\"shared_check_timeouts\":1,\"shared_check_timeout_retries\":1,\"shared_check_recoveries\":[{\"name\":\"check-a\",\"command\":\"cargo test --lib check_a\",\"detail\":\"first run timed out after 300s\"}],\"sources\":[]}",
        ),
        at: 1,
    });
    state.jobs.insert(String::from("merge-recovered"), job);

    assert_eq!(
        timeout_status_line(&state),
        "shared-check timeouts 1, timeout retries 1, recovered checks 1"
    );
}

#[test]
fn exhausted_worker_timeout_retry_records_command_and_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _) = test_ctx(dir.path(), PumpConfig::default());
    let job = PoolJob::from(JobSpec::new("merge-1", "merge", "merge", "s", "b"));
    let receipt = MergeReceipt {
        version: 1,
        target: String::from("default"),
        tip: String::from("0123456789012345678901234567890123456789"),
        shared_check_timeouts: Some(2),
        shared_check_timeout_retries: Some(1),
        shared_check_recoveries: Some(Vec::new()),
        sources: vec![SourceResult {
            kind: String::from("unit"),
            id: String::from("u1"),
            status: String::from("undecided"),
            why: String::from("shared-check-timeout"),
            detail: String::from(
                "shared check cargo test --workspace timed out after 900s; its retry also timed out",
            ),
            outside: Vec::new(),
            secs: None,
        }],
    };

    record_shared_timeout_exception(&ctx, &job, &receipt);

    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        exceptions.contains("shared-check-timeout merge-1"),
        "{exceptions}"
    );
    assert!(
        exceptions.contains("cargo test --workspace"),
        "{exceptions}"
    );
    assert!(exceptions.contains("timed out after 900s"), "{exceptions}");
}

#[test]
fn accepted_shared_timeout_result_requeues_without_opening_a_repair() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _) = test_ctx(dir.path(), PumpConfig::default());
    let mut job = PoolJob::from(JobSpec::new("merge-1", "merge", "merge", "s", "b"));
    job.units = vec![String::from("u1")];
    let mut state = State::default();
    state.units.insert(
        String::from("u1"),
        Unit {
            status: Status::Merging,
            ..Unit::default()
        },
    );
    let source = SourceResult {
        kind: String::from("unit"),
        id: String::from("u1"),
        status: String::from("undecided"),
        why: String::from("shared-check-timeout"),
        detail: String::from("cargo check timed out after 300s; retry timed out"),
        outside: Vec::new(),
        secs: None,
    };

    apply_source(
        &ctx,
        &mut state,
        &job,
        &source,
        "0123456789012345678901234567890123456789",
    );

    assert_eq!(state.units["u1"].status, Status::Delivered);
    assert_eq!(state.units["u1"].repairs, None);
    assert_eq!(state.units["u1"].quarantine, None);
}
