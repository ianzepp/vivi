use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use super::need_audit::*;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, RealHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob, enqueue};
use crate::pump::need;
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};
use sha2::{Digest, Sha256};

const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const TIP: &str = "1123456789abcdef0123456789abcdef01234567";
const REPAIR_TIP: &str = "2123456789abcdef0123456789abcdef01234567";
const SOURCE_HASH: &str = "v3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RESULT_PREFIX: &str = "PUMP_NEED_AUDIT_RESULT=";

fn binding(tip: &str) -> NeedAuditBinding {
    NeedAuditBinding::new("need-1", "default", 1, BASE, tip)
}

fn requirement() -> NeedAuditRequirement {
    NeedAuditRequirement {
        id: String::from("req-1"),
        text: String::from("the delivered behavior works"),
    }
}

fn expectation(binding: NeedAuditBinding, audit_base: &str, round: u32) -> NeedAuditExpectation {
    expectation_with_hash(binding, audit_base, round, SOURCE_HASH)
}

fn expectation_with_hash(
    binding: NeedAuditBinding,
    audit_base: &str,
    round: u32,
    source_hash: &str,
) -> NeedAuditExpectation {
    let job_id = need_audit_job_id(&binding, source_hash, audit_base, round);
    NeedAuditExpectation {
        binding,
        source_hash: source_hash.to_owned(),
        job_id,
        audit_base: audit_base.to_owned(),
        round,
        requirements: vec![requirement()],
    }
}

fn result(
    expected: &NeedAuditExpectation,
    verdict: NeedAuditVerdict,
    status: NeedRequirementStatus,
) -> NeedAuditReceipt {
    let findings = if matches!(
        verdict,
        NeedAuditVerdict::BlockRange | NeedAuditVerdict::BlockShip
    ) {
        vec![NeedAuditFinding {
            id: String::from("finding-1"),
            content: String::from("the requirement is not met"),
            paths: vec![String::from("src/lib.rs")],
            recheck_trigger: String::from("review the repair commit"),
        }]
    } else {
        Vec::new()
    };
    NeedAuditReceipt {
        version: 1,
        receipt_key: receipt_key(expected),
        job_id: expected.job_id.clone(),
        binding: expected.binding.clone(),
        source_hash: expected.source_hash.clone(),
        audit_base: expected.audit_base.clone(),
        round: expected.round,
        verdict,
        requirements: vec![NeedAuditRequirementResult {
            id: requirement().id,
            text: requirement().text,
            status,
            evidence: vec![String::from("focused check output is recorded")],
        }],
        findings,
        diagnoses: if verdict == NeedAuditVerdict::Residual {
            vec![String::from("bounded evidence gap remains")]
        } else {
            Vec::new()
        },
        blind_spots: Vec::new(),
        report: NeedAuditReportProvenance {
            id: String::from("report-1"),
            reference: String::from("task-reply"),
            task: None,
            pool: None,
            collected_at: None,
        },
    }
}

fn with_source(mut receipt: NeedAuditReceipt) -> NeedAuditReceipt {
    receipt.report.task = Some(String::from("task-1234abcd"));
    receipt.report.pool = Some(String::from("audit-pool"));
    receipt.report.collected_at = Some(99);
    receipt
}

fn thread_messages() -> serde_json::Value {
    serde_json::json!([
        {
            "handle": "need-1",
            "message_id": "message-1",
            "from": "mind@local",
            "subject": "Implement behavior",
            "body": "Need: implement the delivered behavior\ndone_when: the delivered behavior works"
        },
        {
            "handle": "reply-1",
            "message_id": "message-2",
            "from": "operator@local",
            "subject": "Need amendment",
            "body": "operator reply: retain the CLI shape"
        },
        {
            "handle": "reply-2",
            "message_id": "message-3",
            "from": "mind@local",
            "subject": "Pump plan",
            "body": "pump: planned at revision 1"
        }
    ])
}

fn reply_with_audit_sources(host: &FakeHost) {
    host.reply(
        "need show",
        0,
        &serde_json::to_string(&thread_messages()).unwrap(),
    );
    host.reply("merge-base --is-ancestor", 0, "");
    host.reply("git diff --name-only --no-renames -z", 0, "src/lib.rs\0");
    host.reply(
        "ls-remote --exit-code",
        0,
        &format!("{TIP}\trefs/heads/vivi/need/need-1\n"),
    );
}

fn thread_hash(ctx: &crate::pump::host::Ctx) -> String {
    source_hash_for(ctx, &thread_messages())
}

fn source_hash_for(ctx: &crate::pump::host::Ctx, messages: &serde_json::Value) -> String {
    crate::pump::amend::source_thread_hash(ctx, "need-1", messages.as_array().unwrap()).unwrap()
}

fn legacy_mind_only_hash() -> String {
    format!("v2:{}", hex::encode(Sha256::digest(b"")))
}

fn frozen_state(binding: &NeedAuditBinding, thread_hash: String) -> State {
    let mut st = State::default();
    st.needs.insert(
        binding.need.clone(),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("unit-1")],
            revision: binding.revision,
            thread_sha256: Some(thread_hash),
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        String::from("unit-1"),
        Unit {
            need: binding.need.clone(),
            target: binding.target.clone(),
            revision: binding.revision,
            title: String::from("implement behavior"),
            brief: String::from("implement and validate the requested behavior"),
            status: Status::Merged,
            branch: Some(String::from("vivi/unit/unit-1-a1")),
            commit: Some(binding.tip.clone()),
            ..Unit::default()
        },
    );
    freeze_branch(&mut st, binding);
    st
}

fn freeze_branch(st: &mut State, binding: &NeedAuditBinding) {
    need::reserve(
        st,
        &binding.target,
        &binding.need,
        binding.revision,
        &binding.base,
    )
    .unwrap();
    need::record_create_job(st, &binding.target, &binding.need, "create-job").unwrap();
    need::record_created(
        st,
        &binding.target,
        &binding.need,
        "create-job",
        &binding.base,
        &binding.base,
    )
    .unwrap();
    need::record_tip(
        st,
        &binding.target,
        &binding.need,
        binding.revision,
        &binding.base,
        &binding.tip,
    )
    .unwrap();
    need::freeze(
        st,
        &binding.target,
        &binding.need,
        binding.revision,
        &binding.tip,
    )
    .unwrap();
}

fn routed_context(
    root: &std::path::Path,
) -> (crate::pump::host::Ctx, tempfile::TempDir, Arc<FakeHost>) {
    let dir = tempfile::tempdir_in(root).unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let routing = RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("auditor"),
            kind: Some(String::from("auditor")),
            status: String::from("active"),
            pool: Some(String::from("audit-pool")),
            read_only: Some(true),
            model: Some(String::from("role-configured-model")),
            models: Vec::new(),
        }],
        pools: BTreeMap::from([
            (
                String::from("audit-pool"),
                RoutedPool {
                    slots: 1,
                    provider: String::from("local"),
                    fallback: Some(String::from("audit-fallback")),
                    caps: BTreeMap::new(),
                    models: Vec::new(),
                    has_bands: false,
                },
            ),
            (
                String::from("audit-fallback"),
                RoutedPool {
                    slots: 1,
                    provider: String::from("local"),
                    fallback: None,
                    caps: BTreeMap::new(),
                    models: Vec::new(),
                    has_bands: false,
                },
            ),
        ]),
    };
    (ctx.with_routing(routing), dir, host)
}

#[test]
fn frozen_need_enqueues_one_durable_job_with_full_context_and_exact_range() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _dir, host) = routed_context(dir.path());
    reply_with_audit_sources(&host);
    let binding = binding(TIP);
    let mut st = frozen_state(&binding, thread_hash(&ctx));
    let id = dispatch_frozen(&ctx, &mut st, binding.clone(), BASE, 1, "").unwrap();
    let job = &st.jobs[&id];

    assert_eq!(job.kind, "need-audit");
    assert_eq!(job.role, "auditor");
    assert_eq!(job.pool_candidates, ["audit-pool", "audit-fallback"]);
    assert_eq!(job.target.as_deref(), Some("default"));
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert_eq!(job.with, [BASE]);
    assert!(job.read_only);
    assert!(
        job.model.is_none(),
        "the normal role routing owns model selection"
    );
    assert!(job.body.contains("frozen_binding="));
    assert!(job.body.contains(&format!("review_range={BASE}..{TIP}")));
    assert!(job.body.contains("operator reply: retain the CLI shape"));
    assert!(job.body.contains("Need: implement the delivered behavior"));
    assert!(!job.body.contains("pump: planned at revision 1"));
    assert!(
        job.body
            .contains("implement and validate the requested behavior")
    );
    assert!(job.body.contains("the delivered behavior works"));
    assert!(job.body.contains("src/lib.rs"));
    assert_eq!(
        expectation_from_job(job).unwrap().requirements,
        [NeedAuditRequirement {
            id: String::from("need-1-r1"),
            text: String::from("the delivered behavior works"),
        }]
    );
    assert_eq!(st.jobs.len(), 1);
}

#[test]
fn repeating_the_same_dispatch_is_durable_and_changed_input_cannot_reuse_it() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _dir, host) = routed_context(dir.path());
    reply_with_audit_sources(&host);
    let binding = binding(TIP);
    let mut st = frozen_state(&binding, thread_hash(&ctx));
    let first = dispatch_frozen(&ctx, &mut st, binding.clone(), BASE, 1, "").unwrap();
    let saved = serde_json::to_vec(&st).unwrap();
    let mut recovered: State = serde_json::from_slice(&saved).unwrap();
    let second = dispatch_frozen(&ctx, &mut recovered, binding.clone(), BASE, 1, "").unwrap();

    assert_eq!(first, second);
    assert_eq!(recovered.jobs.len(), 1);
    let original_body = recovered.jobs[&first].body.clone();
    host.reply(
        "need show",
        0,
        &serde_json::to_string(&serde_json::json!([
            thread_messages().as_array().unwrap()[0].clone(),
            thread_messages().as_array().unwrap()[1].clone(),
            thread_messages().as_array().unwrap()[2].clone(),
            {
                "handle": "reply-3",
                "message_id": "message-4",
                "from": "mind@local",
                "subject": "Later pump status",
                "body": "pump: attempt record\noutcome: delivered"
            }
        ]))
        .unwrap(),
    );
    let third = dispatch_frozen(&ctx, &mut recovered, binding.clone(), BASE, 1, "").unwrap();
    assert_eq!(third, first);
    assert_eq!(recovered.jobs[&first].body, original_body);
    assert_eq!(
        host.logs()
            .iter()
            .filter(|line| line.contains("need acceptance audit queued"))
            .count(),
        1
    );
    host.reply(
        "need show",
        0,
        &serde_json::to_string(&serde_json::json!([{
            "handle": "need-1",
            "message_id": "message-1",
            "from": "operator@local",
            "subject": "Implement behavior",
            "body": "Need: implement the delivered behavior\ndone_when: changed requirement"
        }]))
        .unwrap(),
    );
    assert!(dispatch_frozen(&ctx, &mut recovered, binding, BASE, 1, "").is_err());
}

#[test]
fn each_supported_verdict_and_requirement_status_keeps_its_structured_shape() {
    let expected = expectation(binding(TIP), BASE, 1);
    for verdict in [
        NeedAuditVerdict::CleanPass,
        NeedAuditVerdict::Residual,
        NeedAuditVerdict::BlockRange,
        NeedAuditVerdict::BlockShip,
    ] {
        let report = result(&expected, verdict, NeedRequirementStatus::Satisfied);
        validate_receipt(&report, &expected).unwrap();
        let encoded = serde_json::to_value(report).unwrap();
        assert_eq!(encoded["verdict"], serde_json::to_value(verdict).unwrap());
    }
    for status in [
        NeedRequirementStatus::Satisfied,
        NeedRequirementStatus::Partial,
        NeedRequirementStatus::Missing,
        NeedRequirementStatus::Unverified,
        NeedRequirementStatus::NotApplicable,
    ] {
        let report = result(&expected, NeedAuditVerdict::Residual, status);
        validate_receipt(&report, &expected).unwrap();
        assert_eq!(report.requirements[0].status, status);
    }
}

#[test]
fn a_marked_receipt_parses_and_keeps_report_and_blind_spot_data() {
    let expected = expectation(binding(TIP), BASE, 1);
    let mut report = result(
        &expected,
        NeedAuditVerdict::Residual,
        NeedRequirementStatus::Unverified,
    );
    report
        .blind_spots
        .push(String::from("the target CI run is unavailable"));
    let json = serde_json::to_string(&report).unwrap();
    let raw = format!("agent_rc 0\nhand's final message:\n{RESULT_PREFIX}{json}\n");
    let parsed = parse_receipt(&raw, &expected).unwrap();

    assert_eq!(parsed, report);
    assert_eq!(parsed.blind_spots, ["the target CI run is unavailable"]);
    assert_eq!(parsed.report.reference, "task-reply");
}

#[test]
fn stale_binding_range_round_and_requirement_reports_are_rejected() {
    let expected = expectation(binding(TIP), BASE, 1);
    let mut wrong_tip = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    );
    wrong_tip.binding.tip = REPAIR_TIP.to_owned();
    assert!(validate_receipt(&wrong_tip, &expected).is_err());

    let mut wrong_base_binding = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    );
    wrong_base_binding.binding.base = REPAIR_TIP.to_owned();
    assert!(validate_receipt(&wrong_base_binding, &expected).is_err());

    let mut wrong_target = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    );
    wrong_target.binding.target = String::from("another-target");
    assert!(validate_receipt(&wrong_target, &expected).is_err());

    let mut wrong_revision = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    );
    wrong_revision.binding.revision += 1;
    assert!(validate_receipt(&wrong_revision, &expected).is_err());

    let mut wrong_base = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    );
    wrong_base.audit_base = REPAIR_TIP.to_owned();
    assert!(validate_receipt(&wrong_base, &expected).is_err());

    let mut wrong_round = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    );
    wrong_round.round = 2;
    assert!(validate_receipt(&wrong_round, &expected).is_err());

    let mut wrong_source = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    );
    wrong_source.source_hash = format!("v3:{}", "b".repeat(64));
    assert!(validate_receipt(&wrong_source, &expected).is_err());

    let mut omitted = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    );
    omitted.requirements.clear();
    assert!(validate_receipt(&omitted, &expected).is_err());
}

#[test]
fn missing_or_malformed_result_markers_are_rejected() {
    let expected = expectation(binding(TIP), BASE, 1);
    assert!(parse_receipt("no result", &expected).is_err());
    let raw = format!("hand's final message:\n{RESULT_PREFIX}{{bad json}}\n");
    assert!(parse_receipt(&raw, &expected).is_err());
}

#[test]
fn a_clean_pass_cannot_hide_unverified_requirements_or_findings() {
    let expected = expectation(binding(TIP), BASE, 1);
    let report = result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Unverified,
    );
    assert!(validate_receipt(&report, &expected).is_err());
}

#[test]
fn durable_job_payload_reconstructs_the_same_receipt_expectation_after_restart() {
    let binding = binding(TIP);
    let expected = expectation(binding.clone(), BASE, 1);
    let mut spec = JobSpec::new(
        expected.job_id.clone(),
        "need-audit",
        "auditor",
        "acceptance audit need-1",
        "audit packet",
    );
    spec.payload = serde_json::json!({
        "binding": binding,
        "source_hash": expected.source_hash,
        "audit_base": BASE,
        "round": 1,
        "requirements": [requirement()],
    });
    let mut st = State::default();
    enqueue(&mut st, spec.clone()).unwrap();
    enqueue(&mut st, spec).unwrap();

    let recovered = expectation_from_job(&st.jobs[&expected.job_id]).unwrap();
    assert_eq!(recovered, expected);
    assert_eq!(st.jobs.len(), 1);
}

#[test]
fn collected_job_receipt_uses_controller_task_and_pool_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _dir, host) = routed_context(dir.path());
    reply_with_audit_sources(&host);
    let binding = binding(TIP);
    let expected = expectation_with_hash(binding.clone(), BASE, 1, &thread_hash(&ctx));
    let mut report = result(
        &expected,
        NeedAuditVerdict::Residual,
        NeedRequirementStatus::Unverified,
    );
    report.report.task = Some(String::from("forged-task"));
    report.report.pool = Some(String::from("forged-pool"));
    report.report.collected_at = Some(1);
    let response = format!(
        "agent exit 0 in 1s\nagent_rc 0\nhand's final message:\n{RESULT_PREFIX}{}\n",
        serde_json::to_string(&report).unwrap()
    );
    let mut spec = JobSpec::new(
        expected.job_id.clone(),
        "need-audit",
        "auditor",
        "acceptance audit need-1",
        "audit packet",
    );
    spec.payload = serde_json::json!({
        "binding": expected.binding,
        "source_hash": expected.source_hash,
        "audit_base": expected.audit_base,
        "round": expected.round,
        "requirements": expected.requirements,
    });
    let mut job = PoolJob::from(spec);
    job.task = Some(String::from("task-1234abcd"));
    job.pool = Some(String::from("audit-pool"));
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent_rc 0"),
        receipt: response,
        at: 99,
    });

    let parsed = parse_job_receipt(&job).unwrap();

    assert_eq!(parsed.report.task.as_deref(), Some("task-1234abcd"));
    assert_eq!(parsed.report.pool.as_deref(), Some("audit-pool"));
    assert_eq!(parsed.report.collected_at, Some(99));
    let mut st = frozen_state(&binding, thread_hash(&ctx));
    validate_current_job_receipt(&ctx, &st, &job, &parsed).unwrap();
    st.needs.get_mut(&binding.need).unwrap().revision += 1;
    assert!(validate_current_job_receipt(&ctx, &st, &job, &parsed).is_err());
}

#[test]
fn current_binding_rejects_acceptance_after_unit_membership_changes() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _dir, host) = routed_context(dir.path());
    reply_with_audit_sources(&host);
    let binding = binding(TIP);
    let expected = expectation_with_hash(binding.clone(), BASE, 1, &thread_hash(&ctx));
    let receipt = with_source(result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    ));
    let mut st = frozen_state(&binding, thread_hash(&ctx));
    validate_current_receipt(&ctx, &st, &receipt, &expected).unwrap();
    st.needs
        .get_mut(&binding.need)
        .unwrap()
        .units
        .push(String::from("late-unit"));

    assert!(validate_current_receipt(&ctx, &st, &receipt, &expected).is_err());
}

#[test]
fn a_changed_need_branch_tip_or_plan_revision_invalidates_the_old_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _dir, host) = routed_context(dir.path());
    reply_with_audit_sources(&host);
    let binding = binding(TIP);
    let expected = expectation_with_hash(binding.clone(), BASE, 1, &thread_hash(&ctx));
    let receipt = with_source(result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    ));
    let mut st = frozen_state(&binding, thread_hash(&ctx));

    need::observe_branch_tip(&mut st, &binding.target, &binding.need, REPAIR_TIP).unwrap();
    assert!(validate_current_receipt(&ctx, &st, &receipt, &expected).is_err());

    let mut st = frozen_state(&binding, thread_hash(&ctx));
    st.needs.get_mut(&binding.need).unwrap().revision += 1;
    assert!(validate_current_receipt(&ctx, &st, &receipt, &expected).is_err());
}

#[test]
fn repair_review_has_a_new_job_and_receipt_identity_but_keeps_full_binding() {
    let full_binding = binding(REPAIR_TIP);
    let first_round = expectation(binding(TIP), BASE, 1);
    let repair_round = expectation(full_binding.clone(), TIP, 2);

    assert_ne!(first_round.job_id, repair_round.job_id);
    assert_ne!(receipt_key(&first_round), receipt_key(&repair_round));
    assert_eq!(repair_round.binding.base, BASE);
    assert_eq!(repair_round.audit_base, TIP);
}

#[test]
fn a_mind_authored_amendment_invalidates_a_job_even_under_a_legacy_plan_hash() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _dir, host) = routed_context(dir.path());
    let original_thread = serde_json::json!([{
        "handle": "need-1",
        "message_id": "mind-root",
        "from": "mind@local",
        "subject": "Need",
        "body": "Need\ndone_when: original requirement"
    }, {
        "handle": "need-1-reply",
        "message_id": "pump-reply",
        "from": "mind@local",
        "subject": "Pump plan",
        "body": "pump: planned at revision 1"
    }]);
    host.reply(
        "need show",
        0,
        &serde_json::to_string(&original_thread).unwrap(),
    );
    host.reply(
        "ls-remote --exit-code",
        0,
        &format!("{TIP}\trefs/heads/vivi/need/need-1\n"),
    );
    let binding = binding(TIP);
    let old_source_hash = source_hash_for(&ctx, &original_thread);
    let expected = expectation_with_hash(binding.clone(), BASE, 1, &old_source_hash);
    let receipt = with_source(result(
        &expected,
        NeedAuditVerdict::CleanPass,
        NeedRequirementStatus::Satisfied,
    ));
    let st = frozen_state(&binding, legacy_mind_only_hash());
    let amended_thread = serde_json::json!([{
        "handle": "need-1",
        "message_id": "mind-root",
        "from": "mind@local",
        "subject": "Need",
        "body": "Need\ndone_when: original requirement"
    }, {
        "handle": "need-1-reply",
        "message_id": "pump-reply",
        "from": "mind@local",
        "subject": "Pump plan",
        "body": "pump: planned at revision 1"
    }, {
        "handle": "need-1-amendment",
        "message_id": "mind-amendment",
        "from": "mind@local",
        "subject": "Need amendment",
        "body": "done_when: added requirement"
    }]);
    host.reply(
        "need show",
        0,
        &serde_json::to_string(&amended_thread).unwrap(),
    );

    let fresh_hash = source_hash_for(&ctx, &amended_thread);
    assert_ne!(fresh_hash, old_source_hash);
    assert!(
        validate_current_receipt(&ctx, &st, &receipt, &expected)
            .unwrap_err()
            .contains("source-thread snapshot")
    );
}

#[test]
fn cold_target_fetches_only_frozen_need_metadata_before_inventoring_paths() {
    let root = tempfile::tempdir().unwrap();
    let remote = root.path().join("origin.git");
    let seed = root.path().join("seed");
    let target = root.path().join("target");
    run_git(
        root.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=next",
            remote.to_str().unwrap(),
        ],
    );
    run_git(
        root.path(),
        &["init", "--initial-branch=next", seed.to_str().unwrap()],
    );
    run_git(&seed, &["config", "user.name", "Audit test"]);
    run_git(&seed, &["config", "user.email", "audit@example.test"]);
    fs::write(seed.join("src.rs"), "base\n").unwrap();
    run_git(&seed, &["add", "src.rs"]);
    run_git(&seed, &["commit", "-m", "base"]);
    run_git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    run_git(&seed, &["push", "origin", "next"]);
    let base = run_git(&seed, &["rev-parse", "HEAD"]);
    run_git(
        root.path(),
        &[
            "clone",
            "--branch",
            "next",
            remote.to_str().unwrap(),
            target.to_str().unwrap(),
        ],
    );

    run_git(&seed, &["checkout", "-b", "vivi/need/need-1"]);
    fs::write(seed.join("src.rs"), "need branch\n").unwrap();
    run_git(&seed, &["commit", "-am", "need branch tip"]);
    run_git(&seed, &["push", "origin", "vivi/need/need-1"]);
    let tip = run_git(&seed, &["rev-parse", "HEAD"]);
    assert_ne!(
        run_git_status(&target, &["cat-file", "-e", &format!("{tip}^{{commit}}")]),
        0
    );

    let binding = NeedAuditBinding::new("need-1", "default", 1, &base, &tip);
    let st = frozen_state(&binding, String::from(SOURCE_HASH));
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let mut cfg = PumpConfig::default();
    cfg.remote = String::from("origin");
    let ctx = Ctx::with_parts(
        cfg,
        Arc::new(RealHost::new(&home)),
        root.path().join("project"),
        String::from("vivi"),
        String::new(),
        home,
        target.clone(),
    );
    let head_before = run_git(&target, &["rev-parse", "HEAD"]);
    let fetch_head_before = fs::read(target.join(".git/FETCH_HEAD")).ok();
    let worktree_before = run_git(&target, &["status", "--porcelain"]);

    ensure_audit_range_objects(&ctx, &st, &binding, &base).unwrap();
    let paths = changed_paths(&ctx, &base, &tip).unwrap();

    assert_eq!(paths, ["src.rs"]);
    assert_eq!(run_git(&target, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        fs::read(target.join(".git/FETCH_HEAD")).ok(),
        fetch_head_before
    );
    assert_eq!(
        run_git(&target, &["status", "--porcelain"]),
        worktree_before
    );
    assert_eq!(
        run_git_status(
            &target,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/vivi/need/need-1"
            ]
        ),
        1
    );
    assert_eq!(
        run_git_status(
            &target,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                "refs/remotes/origin/vivi/need/need-1"
            ]
        ),
        1
    );
    assert_eq!(
        run_git(
            &target,
            &[
                "rev-parse",
                "--verify",
                &format!("{}^{{commit}}", audit_metadata_ref(&binding))
            ]
        ),
        tip
    );
}

fn run_git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn run_git_status(cwd: &Path, args: &[&str]) -> i32 {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .code()
        .unwrap_or(128)
}
