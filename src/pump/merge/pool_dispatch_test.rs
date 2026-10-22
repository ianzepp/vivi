use std::collections::{BTreeMap, BTreeSet};

use super::{
    MERGE_BRIEF, MERGE_TIMEOUT_RULES, MergeReceipt, SourceResult, apply_source, parse_receipt,
    parse_receipt_form, published_integration_tip, record_shared_timeout_exception,
    sources_failure, timeout_status_line, valid_sha,
};
use crate::pump::config::{Generated, PumpConfig};
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};
use crate::pump::marked_result::ResultForm;
use crate::pump::merge::{dispatch_pool, target_write_blocked};
use crate::pump::state::{GateRecord, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

fn valid_sources(
    sources: &[SourceResult],
    units: &BTreeSet<String>,
    jobs: &BTreeSet<String>,
) -> bool {
    sources_failure(sources, units, jobs).is_none()
}

const PRODUCTION_RECEIPT: &str = r#"{"version":1,"target":"default","tip":"67e16baadf29ce69ef73baf9300d611310bce38e","sources":[{"kind":"unit","id":"0c7ce8f9-2","status":"merged","why":"","detail":"Clean merge ...","outside":[],"secs":0}],"shared_check_timeouts":0,"shared_check_timeout_retries":0,"shared_check_recoveries":[]}"#;

#[test]
fn merge_receipt_without_prefix_is_found_as_a_standalone_line() {
    let text = format!("Merged cleanly.\n\n{PRODUCTION_RECEIPT}");
    let (receipt, form) = parse_receipt_form(&text).unwrap();
    assert_eq!(form, ResultForm::Unprefixed);
    assert_eq!(receipt.tip, "67e16baadf29ce69ef73baf9300d611310bce38e");
    assert_eq!(receipt.sources[0].id, "0c7ce8f9-2");
}

#[test]
fn merge_receipt_fenced_or_backticked_without_prefix_is_found() {
    let fenced = format!("done\n```json\n{PRODUCTION_RECEIPT}\n```\n");
    assert_eq!(parse_receipt(&fenced).unwrap().target, "default");
    let inline = format!("done\n`{PRODUCTION_RECEIPT}`\n");
    assert_eq!(parse_receipt(&inline).unwrap().target, "default");
    let one_line_fence = format!("done\n```{PRODUCTION_RECEIPT}```\n");
    assert_eq!(parse_receipt(&one_line_fence).unwrap().target, "default");
}

#[test]
fn merge_receipt_prefixed_line_is_preferred_over_a_bare_one() {
    let prefixed = PRODUCTION_RECEIPT.replace("\"default\"", "\"prefixed\"");
    let text = format!("PUMP_MERGE_RESULT={prefixed}\n{PRODUCTION_RECEIPT}\n");
    let (receipt, form) = parse_receipt_form(&text).unwrap();
    assert_eq!(form, ResultForm::Prefixed);
    assert_eq!(receipt.target, "prefixed");
}

#[test]
fn merge_receipt_inside_prose_is_not_parsed() {
    let text = format!("The receipt is {PRODUCTION_RECEIPT} as requested.\n");
    assert!(parse_receipt(&text).is_none());
    let trailing = format!("receipt: {PRODUCTION_RECEIPT}\n");
    assert!(parse_receipt(&trailing).is_none());
}

#[test]
fn merge_receipt_object_without_a_tip_is_not_parsed() {
    let text = "{\"version\":1,\"target\":\"default\",\"sources\":[]}\n";
    assert!(parse_receipt(text).is_none());
    assert!(parse_receipt("{\"status\":\"ok\"}\n").is_none());
}

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
fn an_unattempted_gate_blocks_new_target_writes() {
    let mut state = State::default();
    let source = merged_unapplied_source(None);
    state.jobs.insert(source.id.clone(), source);

    assert!(target_write_blocked(&mut state, "default"));
}

#[test]
fn a_completed_red_gate_allows_a_later_accepted_need_write() {
    let mut state = State::default();
    state.merges = 1;
    state.last_gate.merges = 1;
    state.last_gate.tip = Some(String::from("0123456789012345678901234567890123456789"));
    state.gates.push(GateRecord {
        tip: String::from("0123456789012345678901234567890123456789"),
        at: 1,
        merges: 1,
        names: Some(vec![String::from("known_red_check")]),
        new: Some(vec![String::from("known_red_check")]),
    });
    let source = merged_unapplied_source(Some("0123456789012345678901234567890123456789"));
    state.jobs.insert(source.id.clone(), source);

    assert!(!target_write_blocked(&mut state, "default"));
}

#[test]
fn an_active_gate_or_writer_still_blocks_new_target_writes() {
    let mut active_gate = State::default();
    active_gate.last_gate.job_id = Some(String::from("gate-active"));
    assert!(target_write_blocked(&mut active_gate, "default"));

    let mut active_writer = State::default();
    let mut writer = PoolJob::from(JobSpec::new("merge-active", "merge", "merge", "s", "b"));
    writer.target = Some(String::from("default"));
    writer.status = JobStatus::Dispatched;
    active_writer.jobs.insert(writer.id.clone(), writer);
    assert!(target_write_blocked(&mut active_writer, "default"));
}

fn merged_unapplied_source(gate_attempted_tip: Option<&str>) -> PoolJob {
    let tip = "0123456789012345678901234567890123456789";
    let mut source = PoolJob::from(JobSpec::new("role-source", "role", "role", "s", "b"));
    source.target = Some(String::from("default"));
    source.integration_required = true;
    source.status = JobStatus::Finished;
    source.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent exit 0"),
        receipt: String::from("published candidate"),
        at: 1,
    });
    source.merged_tip = Some(String::from(tip));
    source.gated_tip = None;
    source.gate_attempted_tip = gate_attempted_tip.map(str::to_owned);
    source.applied_at = None;
    source
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
fn merge_brief_leads_with_diagnosis_not_quarantine() {
    let lead: String = MERGE_BRIEF.lines().take(6).collect::<Vec<_>>().join("\n");
    assert!(
        MERGE_BRIEF.to_ascii_lowercase().contains("diagnos"),
        "merge brief should tell the model to diagnose first: {MERGE_BRIEF}"
    );
    assert!(
        !lead.to_ascii_lowercase().contains("quarantine"),
        "merge brief still leads with quarantine: {lead}"
    );
    assert!(
        !lead.to_ascii_lowercase().contains("bisect"),
        "merge brief still leads with bisect: {lead}"
    );
    let timeout_lead = MERGE_TIMEOUT_RULES
        .split(". ")
        .next()
        .unwrap_or(MERGE_TIMEOUT_RULES);
    assert!(
        !timeout_lead.to_ascii_lowercase().contains("quarantine")
            && !timeout_lead.to_ascii_lowercase().contains("bisect"),
        "timeout rules still lead with quarantine or bisect: {timeout_lead}"
    );
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

#[test]
fn merge_brief_shows_a_job_entry_only_for_listed_role_sources() {
    assert!(!MERGE_BRIEF.contains("\"secs\":0},{\"kind\":\"job\""));
    assert!(MERGE_BRIEF.contains("When `role_sources` is not empty"));
    assert!(MERGE_BRIEF.contains("never for this job itself"));
}
