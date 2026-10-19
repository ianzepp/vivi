use std::collections::{BTreeMap, BTreeSet};

use super::{
    MergeReceipt, SourceResult, parse_receipt, published_integration_tip, valid_sha, valid_sources,
};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, PoolJob};
use crate::pump::merge::dispatch_pool;
use crate::pump::state::State;
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
