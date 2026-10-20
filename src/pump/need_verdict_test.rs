//! Focused tests for frozen need-audit verdict routing.

use std::collections::BTreeMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, enqueue};
use crate::pump::need::{self, NeedAuditBinding, NeedBranchStatus};
use crate::pump::need_audit::{
    self, NeedAuditFinding, NeedAuditReceipt, NeedAuditReportProvenance, NeedAuditRequirement,
    NeedAuditRequirementResult, NeedAuditVerdict, NeedRequirementStatus,
};
use crate::pump::need_verdict::{self, NeedVerdictState, RouteOutcome};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

const NEED: &str = "need1234";
const UNIT: &str = "unit1234";
const BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const REPAIRED_TIP: &str = "cccccccccccccccccccccccccccccccccccccccc";
const NEED_BODY: &str = "ship the requested behavior\ndone_when: the behavior is complete";
const MESSAGE_ID: &str = "<need1234@example.test>";

#[test]
fn clean_pass_accepts_only_the_current_full_binding_and_replays_once() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_context(dir.path());
    let mut state = frozen_state();
    let binding = need::binding_for(&state, "default", NEED).unwrap().unwrap();
    let job = audit_job(&mut state, &binding, BASE, 1);
    let receipt = receipt(&mut state, &job, NeedAuditVerdict::CleanPass, true, false);

    assert_eq!(
        need_verdict::route_receipt(&ctx, &mut state, "default", &job, &receipt).unwrap(),
        RouteOutcome::Applied
    );
    let accepted = need_verdict::current_accepted_receipt(&ctx, &state, NEED, "default")
        .expect("a current clean_pass accepts the frozen range");
    assert_eq!(accepted.receipt, receipt);
    assert_eq!(accepted.job_id, job);
    assert_eq!(
        need_verdict::route_receipt(&ctx, &mut state, "default", &accepted.job_id, &receipt)
            .unwrap(),
        RouteOutcome::Duplicate
    );

    need::observe_branch_tip(&mut state, "default", NEED, REPAIRED_TIP).unwrap();
    assert!(need_verdict::current_accepted_receipt(&ctx, &state, NEED, "default").is_none());
    assert_eq!(
        host.commands()
            .iter()
            .filter(|cmd| cmd.contains("need send"))
            .count(),
        0
    );
}

#[test]
fn residual_accepts_and_files_one_provenanced_need_per_finding_and_requirement_gap() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_context(dir.path());
    let mut state = frozen_state();
    let binding = need::binding_for(&state, "default", NEED).unwrap().unwrap();
    let job = audit_job(&mut state, &binding, BASE, 1);
    let receipt = receipt(&mut state, &job, NeedAuditVerdict::Residual, false, true);

    need_verdict::route_receipt(&ctx, &mut state, "default", &job, &receipt).unwrap();
    assert_eq!(
        need_verdict::route_receipt(&ctx, &mut state, "default", &job, &receipt).unwrap(),
        RouteOutcome::Duplicate
    );
    let routed = need_verdict::state(&state, "default", NEED).unwrap();
    assert_eq!(routed.residual_needs.len(), 2);
    assert!(need_verdict::current_accepted_receipt(&ctx, &state, NEED, "default").is_some());
    let sent = host
        .commands()
        .into_iter()
        .filter(|command| command.contains("need send"))
        .collect::<Vec<_>>();
    assert_eq!(sent.len(), 2);
    for path in [dir.path().join("home/need-audit")] {
        assert!(path.is_dir());
    }
    let body_text = std::fs::read_dir(dir.path().join("home/need-audit"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .collect::<String>();
    assert!(body_text.contains(&receipt.report.reference));
    assert!(body_text.contains("Audit handle:"));
    assert!(body_text.contains("Affected paths:"));
    assert!(body_text.contains("Recheck trigger:"));
    assert!(body_text.contains("UNVERIFIED"));
}

#[test]
fn block_range_starts_same_branch_repair_and_reaudits_only_the_repair_diff() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_context(dir.path());
    let mut state = frozen_state();
    let binding = need::binding_for(&state, "default", NEED).unwrap().unwrap();
    let job = audit_job(&mut state, &binding, BASE, 1);
    let receipt = receipt(&mut state, &job, NeedAuditVerdict::BlockRange, true, true);

    need_verdict::route_receipt(&ctx, &mut state, "default", &job, &receipt).unwrap();
    let routed = need_verdict::state(&state, "default", NEED).unwrap();
    let unit_id = routed.repair_units.values().next().unwrap().clone();
    assert!(state.units.contains_key(&unit_id));
    assert_eq!(state.units[&unit_id].need, NEED);
    assert_eq!(state.units[&unit_id].target, "default");
    assert_eq!(
        state.units[&unit_id].write_scope,
        vec![String::from("src/a.rs")]
    );
    assert_eq!(
        need::get(&state, "default", NEED).unwrap().unwrap().status,
        NeedBranchStatus::Active
    );
    assert_eq!(routed.recheck_base.as_deref(), Some(TIP));
    assert_eq!(routed.recheck_round, 2);
    assert!(
        routed
            .recheck_context
            .as_deref()
            .unwrap()
            .contains("recheck")
    );
    assert!(need_verdict::current_accepted_receipt(&ctx, &state, NEED, "default").is_none());

    state.units.get_mut(&unit_id).unwrap().status = Status::Merged;
    state.units.get_mut(&unit_id).unwrap().commit = Some(REPAIRED_TIP.to_owned());
    need::record_tip(&mut state, "default", NEED, 1, BASE, REPAIRED_TIP).unwrap();
    let repaired_binding = need::freeze(&mut state, "default", NEED, 1, REPAIRED_TIP).unwrap();
    advertise_need_tip(&host, REPAIRED_TIP);
    let request = need_verdict::pending_reaudit(&state, "default", NEED)
        .unwrap()
        .expect("settled repair produces one targeted re-review");
    assert_eq!(request.binding, repaired_binding);
    assert_eq!(request.audit_base, TIP);
    assert_eq!(request.binding.base, BASE);
    assert_eq!(request.binding.tip, REPAIRED_TIP);
    assert_eq!(request.round, 2);
}

#[test]
fn block_range_honors_configured_repair_rounds() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ctx, host) = test_context(dir.path());
    let mut cfg = (*ctx.cfg).clone();
    cfg.need.repair_rounds = 2;
    ctx.cfg = std::sync::Arc::new(cfg);
    let mut state = frozen_state();
    let binding = need::binding_for(&state, "default", NEED).unwrap().unwrap();
    let first_job = audit_job(&mut state, &binding, BASE, 1);
    let first = receipt(
        &mut state,
        &first_job,
        NeedAuditVerdict::BlockRange,
        true,
        true,
    );
    need_verdict::route_receipt(&ctx, &mut state, "default", &first_job, &first).unwrap();
    assert!(
        need_verdict::state(&state, "default", NEED)
            .unwrap()
            .hold
            .is_none()
    );

    let repair_id = need_verdict::state(&state, "default", NEED)
        .unwrap()
        .repair_units["r1:finding-a"]
        .clone();
    state.units.get_mut(&repair_id).unwrap().status = Status::Merged;
    state.units.get_mut(&repair_id).unwrap().commit = Some(REPAIRED_TIP.to_owned());
    need::record_tip(&mut state, "default", NEED, 1, BASE, REPAIRED_TIP).unwrap();
    let binding = need::freeze(&mut state, "default", NEED, 1, REPAIRED_TIP).unwrap();
    advertise_need_tip(&host, REPAIRED_TIP);
    let second_job = audit_job(&mut state, &binding, TIP, 2);
    let second = receipt(
        &mut state,
        &second_job,
        NeedAuditVerdict::BlockRange,
        true,
        true,
    );
    need_verdict::route_receipt(&ctx, &mut state, "default", &second_job, &second).unwrap();
    let routed = need_verdict::state(&state, "default", NEED).unwrap();
    assert!(routed.hold.is_none(), "round 2 should still repair");
    assert_eq!(routed.repair_counts["r1:finding-a"], 2);

    let third_tip = "dddddddddddddddddddddddddddddddddddddddd";
    let second_repair = routed.repair_units["r1:finding-a"].clone();
    state.units.get_mut(&second_repair).unwrap().status = Status::Merged;
    state.units.get_mut(&second_repair).unwrap().commit = Some(third_tip.to_owned());
    need::record_tip(&mut state, "default", NEED, 1, BASE, third_tip).unwrap();
    let binding = need::freeze(&mut state, "default", NEED, 1, third_tip).unwrap();
    advertise_need_tip(&host, third_tip);
    let third_job = audit_job(&mut state, &binding, REPAIRED_TIP, 3);
    let third = receipt(
        &mut state,
        &third_job,
        NeedAuditVerdict::BlockRange,
        true,
        true,
    );
    need_verdict::route_receipt(&ctx, &mut state, "default", &third_job, &third).unwrap();
    let routed = need_verdict::state(&state, "default", NEED).unwrap();
    assert!(routed.hold.is_some());
    assert_eq!(routed.repair_counts["r1:finding-a"], 2);
}

#[test]
fn block_range_caps_repeat_finding_after_one_repair_recheck_and_escalates() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_context(dir.path());
    let mut state = frozen_state();
    let binding = need::binding_for(&state, "default", NEED).unwrap().unwrap();
    let first_job = audit_job(&mut state, &binding, BASE, 1);
    let first = receipt(
        &mut state,
        &first_job,
        NeedAuditVerdict::BlockRange,
        true,
        true,
    );
    need_verdict::route_receipt(&ctx, &mut state, "default", &first_job, &first).unwrap();

    let repair_id = need_verdict::state(&state, "default", NEED)
        .unwrap()
        .repair_units["r1:finding-a"]
        .clone();
    state.units.get_mut(&repair_id).unwrap().status = Status::Merged;
    state.units.get_mut(&repair_id).unwrap().commit = Some(REPAIRED_TIP.to_owned());
    need::record_tip(&mut state, "default", NEED, 1, BASE, REPAIRED_TIP).unwrap();
    let binding = need::freeze(&mut state, "default", NEED, 1, REPAIRED_TIP).unwrap();
    advertise_need_tip(&host, REPAIRED_TIP);
    let second_job = audit_job(&mut state, &binding, TIP, 2);
    let second = receipt(
        &mut state,
        &second_job,
        NeedAuditVerdict::BlockRange,
        true,
        true,
    );

    need_verdict::route_receipt(&ctx, &mut state, "default", &second_job, &second).unwrap();
    let routed = need_verdict::state(&state, "default", NEED).unwrap();
    assert!(routed.hold.is_some());
    assert_eq!(routed.repair_units.len(), 1);
    assert_eq!(routed.ranges.len(), 2);
    assert_eq!(routed.ranges[0].audit_base, BASE);
    assert_eq!(routed.ranges[0].binding.tip, TIP);
    assert_eq!(routed.ranges[1].audit_base, TIP);
    assert_eq!(routed.ranges[1].binding.base, BASE);
    assert_eq!(routed.ranges[1].binding.tip, REPAIRED_TIP);
    assert!(
        routed
            .receipts
            .values()
            .all(|stored| !stored.diagnoses.is_empty())
    );
    assert!(routed.recheck_base.is_none());
    assert!(need_verdict::current_accepted_receipt(&ctx, &state, NEED, "default").is_none());
    assert!(
        host.commands()
            .iter()
            .any(|command| command.contains("--to operator") && command.contains("--to head-cto"))
    );
}

#[test]
fn block_ship_holds_the_branch_and_escalates_once_to_operator_and_head_cto() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_context(dir.path());
    let mut state = frozen_state();
    let binding = need::binding_for(&state, "default", NEED).unwrap().unwrap();
    let job = audit_job(&mut state, &binding, BASE, 1);
    let receipt = receipt(&mut state, &job, NeedAuditVerdict::BlockShip, false, true);

    need_verdict::route_receipt(&ctx, &mut state, "default", &job, &receipt).unwrap();
    let routed = need_verdict::state(&state, "default", NEED).unwrap();
    assert!(routed.hold.is_some());
    assert_eq!(routed.residual_needs.len(), 1);
    assert!(need_verdict::current_accepted_receipt(&ctx, &state, NEED, "default").is_none());
    assert_eq!(
        need_verdict::route_receipt(&ctx, &mut state, "default", &job, &receipt).unwrap(),
        RouteOutcome::Duplicate
    );
    let escalations = host
        .commands()
        .into_iter()
        .filter(|command| command.contains("need send") && command.contains("need audit hold"))
        .count();
    assert_eq!(escalations, 1);
}

#[test]
fn verdict_state_round_trips_without_replacing_other_targets() {
    let mut state = State::default();
    let one = NeedVerdictState {
        original_binding: Some(NeedAuditBinding::new(NEED, "default", 1, BASE, TIP)),
        ..NeedVerdictState::default()
    };
    let two = NeedVerdictState::default();
    need_verdict::set_state(&mut state, "default", NEED, &one).unwrap();
    need_verdict::set_state(&mut state, "gpu", NEED, &two).unwrap();
    assert_eq!(need_verdict::state(&state, "default", NEED).unwrap(), one);
    assert_eq!(need_verdict::state(&state, "gpu", NEED).unwrap(), two);
}

fn test_context(root: &std::path::Path) -> (Ctx, Arc<FakeHost>) {
    let (ctx, host) = test_ctx(root, PumpConfig::default());
    let thread = thread_json();
    host.reply("need show", 0, &thread);
    host.reply("need list", 0, "[]");
    host.reply("need send", 0, "created mind abc12345\nsent abc12345\n");
    advertise_need_tip(&host, TIP);
    host.reply("cat-file -e", 0, "");
    let mut routing: RoleRouting = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("auditor"),
        kind: Some(String::from("auditor")),
        status: String::from("active"),
        pool: Some(String::from("audit-pool")),
        read_only: Some(true),
        model: None,
        models: Vec::new(),
    });
    routing.pools.insert(
        String::from("audit-pool"),
        RoutedPool {
            slots: 2,
            provider: String::from("local"),
            fallback: None,
            caps: BTreeMap::new(),
            models: Vec::new(),
            has_bands: false,
        },
    );
    (ctx.with_routing(routing), host)
}

fn advertise_need_tip(host: &FakeHost, tip: &str) {
    host.reply(
        "ls-remote --exit-code",
        0,
        &format!("{tip}\trefs/heads/vivi/need/{NEED}\n"),
    );
}

fn frozen_state() -> State {
    let mut state = State::default();
    state.needs.insert(
        String::from(NEED),
        NeedRecord {
            verdict: String::from("planned"),
            revision: 1,
            thread_sha256: Some(fixture_source_hash()),
            units: vec![String::from(UNIT)],
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from(UNIT),
        Unit {
            need: String::from(NEED),
            target: String::from("default"),
            revision: 1,
            title: String::from("original unit"),
            brief: String::from("implement the need"),
            write_scope: vec![String::from("src/a.rs")],
            status: Status::Merged,
            since: 1,
            commit: Some(TIP.to_owned()),
            ..Unit::default()
        },
    );
    state.order.push(String::from(UNIT));
    need::reserve(&mut state, "default", NEED, 1, BASE).unwrap();
    need::record_create_job(&mut state, "default", NEED, "need-branch-init").unwrap();
    need::record_created(&mut state, "default", NEED, "need-branch-init", BASE, BASE).unwrap();
    need::record_tip(&mut state, "default", NEED, 1, BASE, TIP).unwrap();
    need::freeze(&mut state, "default", NEED, 1, TIP).unwrap();
    state
}

fn audit_job(
    state: &mut State,
    binding: &NeedAuditBinding,
    audit_base: &str,
    round: u32,
) -> String {
    let source_hash = fixture_source_hash();
    let id = need_audit::need_audit_job_id(binding, &source_hash, audit_base, round);
    let requirement = NeedAuditRequirement {
        id: format!("{NEED}-r1"),
        text: String::from("the behavior is complete"),
    };
    let mut spec = JobSpec::new(
        id.clone(),
        "need-audit",
        "auditor",
        "acceptance review",
        "read-only review",
    );
    spec.target = Some(String::from("default"));
    spec.payload = serde_json::json!({
        "binding": binding,
        "source_hash": source_hash,
        "audit_base": audit_base,
        "round": round,
        "requirements": [requirement],
    });
    enqueue(state, spec).unwrap();
    id
}

fn receipt(
    state: &mut State,
    job_id: &str,
    verdict: NeedAuditVerdict,
    satisfied: bool,
    with_finding: bool,
) -> NeedAuditReceipt {
    let expected = need_audit::expectation_from_job(state.jobs.get(job_id).unwrap()).unwrap();
    let findings = with_finding
        .then(|| {
            vec![NeedAuditFinding {
                id: String::from("finding-a"),
                content: String::from("a scoped defect remains"),
                paths: vec![String::from("src/a.rs")],
                recheck_trigger: String::from("re-review the changed repair diff"),
            }]
        })
        .unwrap_or_default();
    let receipt = NeedAuditReceipt {
        version: 1,
        receipt_key: need_audit::receipt_key(&expected),
        job_id: job_id.to_owned(),
        binding: expected.binding.clone(),
        source_hash: expected.source_hash.clone(),
        audit_base: expected.audit_base.clone(),
        round: expected.round,
        verdict,
        requirements: vec![NeedAuditRequirementResult {
            id: expected.requirements[0].id.clone(),
            text: expected.requirements[0].text.clone(),
            status: if satisfied {
                NeedRequirementStatus::Satisfied
            } else {
                NeedRequirementStatus::Unverified
            },
            evidence: vec![String::from("reviewed against the frozen range")],
        }],
        findings,
        diagnoses: vec![String::from(
            "source-level review of the requested behavior",
        )],
        blind_spots: Vec::new(),
        report: NeedAuditReportProvenance {
            id: String::from("report-1"),
            reference: String::from("audit-report/report-1"),
            task: None,
            pool: None,
            collected_at: None,
        },
    };
    let raw = format!(
        "agent exit 0 in 1s, job 12s, started from 1234567890\n\
         hand's final message:\nPUMP_NEED_AUDIT_RESULT={}\n",
        serde_json::to_string(&receipt).unwrap()
    );
    let job = state.jobs.get_mut(job_id).unwrap();
    job.task = Some(format!("audit-task-{job_id}"));
    job.pool = Some(String::from("audit-pool"));
    job.dispatched_at = Some(1_000);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent exit 0 in 1s"),
        receipt: raw,
        at: 1_001,
    });
    need_audit::parse_job_receipt(job).unwrap()
}

fn thread_json() -> String {
    serde_json::json!([{
        "handle": NEED,
        "message_id": MESSAGE_ID,
        "from": "operator",
        "body": NEED_BODY,
    }])
    .to_string()
}

fn fixture_source_hash() -> String {
    let messages: Vec<serde_json::Value> = serde_json::from_str(&thread_json()).unwrap();
    let mut digest = Sha256::new();
    for message in messages {
        for field in [
            message["handle"].as_str().unwrap_or(""),
            message["message_id"].as_str().unwrap_or(""),
            message["from"].as_str().unwrap_or(""),
            message["subject"].as_str().unwrap_or(""),
            message["body"].as_str().unwrap_or(""),
        ] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
    }
    format!("v3:{}", hex::encode(digest.finalize()))
}
