//! Focused tests for accepted need landing, durable replay, and safe cleanup.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::{collect_completed, dispatch};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus};
use crate::pump::need::{self, NeedAuditBinding};
use crate::pump::need_audit::{
    self, NeedAuditReceipt, NeedAuditReportProvenance, NeedAuditRequirement,
    NeedAuditRequirementResult, NeedAuditVerdict, NeedRequirementStatus,
};
use crate::pump::need_verdict::{self, AcceptedNeedReceipt, NeedVerdictState};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

const NEED: &str = "need1234";
const UNIT: &str = "unit1234";
const BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEED_TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const CANDIDATE: &str = "cccccccccccccccccccccccccccccccccccccccc";
const MESSAGE_ID: &str = "<need1234@example.test>";
const BODY: &str = "ship the requested behavior\ndone_when: the behavior is complete";

struct Fixture {
    _dir: tempfile::TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    state: State,
}

#[test]
fn accepted_need_dispatch_is_durable_and_replay_does_not_duplicate_it() {
    let mut fixture = fixture();
    let id = dispatch(&fixture.ctx, &mut fixture.state)
        .unwrap()
        .pop()
        .expect("accepted frozen need queues a landing job");
    let job = &fixture.state.jobs[&id];
    assert_eq!(job.kind, "need-merge");
    assert!(job.integration_required);
    assert_eq!(job.target.as_deref(), Some("default"));
    assert_eq!(job.from_ref.as_deref(), Some("next"));
    assert_eq!(job.publish_ref.as_deref(), Some("refs/heads/next"));
    assert_eq!(job.publish_contains.as_deref(), Some(NEED_TIP));
    assert_eq!(job.with, vec![String::from(NEED_TIP)]);
    assert_eq!(
        need::get(&fixture.state, "default", NEED)
            .unwrap()
            .unwrap()
            .landing_job_id
            .as_deref(),
        Some(id.as_str())
    );

    let replayed = dispatch(&fixture.ctx, &mut fixture.state).unwrap();
    assert!(replayed.is_empty());
    assert_eq!(
        fixture
            .state
            .jobs
            .values()
            .filter(|job| job.kind == "need-merge")
            .count(),
        1
    );
}

#[test]
fn residual_acceptance_also_authorizes_the_frozen_landing_range() {
    let mut fixture = fixture_with_verdict(NeedAuditVerdict::Residual);

    let ids = dispatch(&fixture.ctx, &mut fixture.state).unwrap();

    assert_eq!(ids.len(), 1);
    assert_eq!(fixture.state.jobs[&ids[0]].kind, "need-merge");
    assert_eq!(fixture.state.jobs[&ids[0]].payload["verdict"], "residual");
}

#[test]
fn publication_without_runner_ancestry_proof_cannot_land_or_queue_cleanup() {
    let mut fixture = fixture();
    let id = queue_landing(&mut fixture);
    let receipt = merge_result(&fixture.state, &id, CANDIDATE);
    let outcome = JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent exit 0"),
        receipt: format!(
            "agent exit 0 in 1s\nPUMP_MERGE_RESULT={receipt}\nrunner publish: pushed refs/heads/next {CANDIDATE} {CANDIDATE}"
        ),
        at: 1_002,
    };
    finish_job(&mut fixture.state, &id, outcome);
    fixture
        .host
        .reply("ls-remote", 0, &format!("{CANDIDATE}\trefs/heads/next\n"));
    reply_frozen_need_ref(&fixture.host);

    collect_completed(&fixture.ctx, &mut fixture.state).unwrap();

    let branch = need::get(&fixture.state, "default", NEED).unwrap().unwrap();
    assert!(branch.landing_receipt.is_none());
    assert_eq!(branch.landing_job_id.as_deref(), Some(id.as_str()));
    assert!(
        fixture
            .state
            .jobs
            .values()
            .all(|job| job.kind != "need-cleanup")
    );
}

#[test]
fn exact_runner_target_and_need_ancestry_receipt_lands_once_and_queues_cleanup() {
    let mut fixture = fixture();
    let id = queue_landing(&mut fixture);
    let receipt = merge_result(&fixture.state, &id, CANDIDATE);
    finish_job(
        &mut fixture.state,
        &id,
        JobOutcome {
            state: String::from("finished"),
            detail: String::from("agent exit 0"),
            receipt: format!(
                "agent exit 0 in 1s\nPUMP_MERGE_RESULT={receipt}\nrunner publish: pushed refs/heads/next {CANDIDATE} {CANDIDATE}\nrunner contains: {NEED_TIP} 1"
            ),
            at: 1_002,
        },
    );
    fixture
        .host
        .reply("ls-remote", 0, &format!("{CANDIDATE}\trefs/heads/next\n"));
    reply_frozen_need_ref(&fixture.host);

    collect_completed(&fixture.ctx, &mut fixture.state).unwrap();
    let branch = need::get(&fixture.state, "default", NEED).unwrap().unwrap();
    let landing = branch.landing_receipt.as_ref().expect("verified landing");
    assert_eq!(landing.binding.tip, NEED_TIP);
    assert_eq!(landing.candidate_sha, CANDIDATE);
    assert_eq!(landing.runner_published_sha, CANDIDATE);
    assert_eq!(landing.observed_target_sha, CANDIDATE);
    assert!(landing.accepted_tip_in_candidate);
    assert!(landing.candidate_in_observed_target);
    assert_eq!(
        fixture.state.jobs[&id].merged_tip.as_deref(),
        Some(CANDIDATE)
    );
    assert!(crate::pump::jobs::sources_waiting_for_gate(&fixture.state, "default").contains(&id));
    fixture.state.baseline = Some(Vec::new());
    crate::pump::gate::start_gate(&fixture.ctx, &mut fixture.state);
    let gate_id = fixture
        .state
        .last_gate
        .job_id
        .as_ref()
        .expect("accepted landing triggers the independent target gate");
    assert_eq!(fixture.state.jobs[gate_id].kind, "gate");
    assert_eq!(
        fixture.state.jobs[gate_id].from_ref.as_deref(),
        Some(CANDIDATE)
    );
    assert!(fixture.state.jobs[gate_id].source_jobs.contains(&id));

    let cleanup_id = dispatch(&fixture.ctx, &mut fixture.state)
        .unwrap()
        .pop()
        .expect("verified landing queues a separate cleanup job");
    let cleanup = &fixture.state.jobs[&cleanup_id];
    assert_eq!(cleanup.kind, "need-cleanup");
    assert!(
        !cleanup.read_only,
        "root cleanup owns a narrow ref deletion capability"
    );
    assert_eq!(cleanup.from_ref.as_deref(), Some(CANDIDATE));
    assert_eq!(cleanup.with, vec![String::from(NEED_TIP)]);
    assert_eq!(
        cleanup.delete_ref.as_deref(),
        Some("refs/heads/vivi/need/need1234")
    );
    assert_eq!(cleanup.delete_expect.as_deref(), Some(NEED_TIP));

    collect_completed(&fixture.ctx, &mut fixture.state).unwrap();
    assert_eq!(
        fixture.state.jobs[&cleanup_id].status,
        JobStatus::Pending,
        "replay keeps the same durable cleanup job"
    );
    assert_eq!(
        fixture
            .state
            .jobs
            .values()
            .filter(|job| job.kind == "need-cleanup")
            .count(),
        1
    );
}

#[test]
fn runner_ref_mismatch_preserves_the_landed_need_branch() {
    let mut fixture = fixture();
    let landing_id = queue_landing(&mut fixture);
    let outcome = successful_merge_outcome(&fixture.state, &landing_id);
    finish_job(&mut fixture.state, &landing_id, outcome);
    fixture
        .host
        .reply("ls-remote", 0, &format!("{CANDIDATE}\trefs/heads/next\n"));
    reply_frozen_need_ref(&fixture.host);
    collect_completed(&fixture.ctx, &mut fixture.state).unwrap();
    let cleanup_id = dispatch(&fixture.ctx, &mut fixture.state)
        .unwrap()
        .pop()
        .unwrap();
    finish_job(
        &mut fixture.state,
        &cleanup_id,
        JobOutcome {
            state: String::from("finished"),
            detail: String::from("agent exit 0"),
            receipt: format!(
                "agent exit 0 in 1s\nrunner delete: ref_mismatch refs/heads/vivi/need/{NEED} {NEED_TIP} {CANDIDATE} {CANDIDATE}"
            ),
            at: 1_003,
        },
    );
    fixture.host.reply(
        "ls-remote",
        0,
        &format!("{CANDIDATE}\trefs/heads/vivi/need/{NEED}\n"),
    );

    collect_completed(&fixture.ctx, &mut fixture.state).unwrap();

    let branch = need::get(&fixture.state, "default", NEED).unwrap().unwrap();
    assert!(branch.landing_receipt.is_some());
    assert!(branch.cleanup_receipt.is_none());
    assert_eq!(branch.status, need::NeedBranchStatus::Frozen);
    assert_eq!(branch.branch, format!("vivi/need/{NEED}"));
    assert_eq!(
        need::cleanup_retry_state(&fixture.state, "default", NEED).unwrap(),
        crate::pump::need::CleanupRetryState::Held
    );
    assert!(
        dispatch(&fixture.ctx, &mut fixture.state)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn exact_runner_delete_and_remote_absence_finish_owned_branch_cleanup() {
    let mut fixture = fixture();
    let landing_id = queue_landing(&mut fixture);
    let outcome = successful_merge_outcome(&fixture.state, &landing_id);
    finish_job(&mut fixture.state, &landing_id, outcome);
    fixture
        .host
        .reply("ls-remote", 0, &format!("{CANDIDATE}\trefs/heads/next\n"));
    reply_frozen_need_ref(&fixture.host);
    collect_completed(&fixture.ctx, &mut fixture.state).unwrap();
    let cleanup_id = dispatch(&fixture.ctx, &mut fixture.state)
        .unwrap()
        .pop()
        .unwrap();
    finish_job(
        &mut fixture.state,
        &cleanup_id,
        JobOutcome {
            state: String::from("finished"),
            detail: String::from("agent exit 0"),
            receipt: format!(
                "agent exit 0 in 1s\nrunner delete: deleted refs/heads/vivi/need/{NEED} {NEED_TIP} {NEED_TIP} absent"
            ),
            at: 1_003,
        },
    );
    fixture
        .host
        .reply("ls-remote", 0, &format!("{CANDIDATE}\trefs/heads/next\n"));

    collect_completed(&fixture.ctx, &mut fixture.state).unwrap();

    let branch = need::get(&fixture.state, "default", NEED).unwrap().unwrap();
    assert_eq!(branch.status, need::NeedBranchStatus::Landed);
    assert!(branch.cleanup_receipt.as_ref().unwrap().verified_absent);
    assert!(need::frozen(&fixture.state).unwrap().is_empty());
    assert!(
        dispatch(&fixture.ctx, &mut fixture.state)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stale_need_thread_acceptance_cannot_queue_a_landing_job() {
    let mut fixture = fixture();
    fixture.host.reply(
        "need show",
        0,
        &thread_json("revised requirement\ndone_when: the revised behavior is complete"),
    );

    assert!(
        dispatch(&fixture.ctx, &mut fixture.state)
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .state
            .jobs
            .values()
            .all(|job| job.kind != "need-merge")
    );
}

fn fixture() -> Fixture {
    fixture_with_verdict(NeedAuditVerdict::CleanPass)
}

fn fixture_with_verdict(verdict: NeedAuditVerdict) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.branch = String::from("next");
    cfg.main = String::from("main");
    cfg.merge.format = String::from("cargo fmt --check");
    cfg.merge.shared_checks = vec![String::from("cargo test --lib")];
    cfg.gate.command = String::from("cargo test --workspace");
    let (base, host) = test_ctx(dir.path(), cfg);
    host.reply("need show", 0, &thread_json(BODY));
    host.reply("need list", 0, "[]");
    reply_frozen_need_ref(&host);
    let mut routing: RoleRouting = (*base.routing).clone();
    routing
        .roles
        .push(role("merge", "merge", "merge-pool", false));
    routing.roles.push(role("gate", "gate", "gate-pool", true));
    routing.pools.insert(String::from("merge-pool"), pool());
    routing.pools.insert(String::from("gate-pool"), pool());
    let ctx = base.with_routing(routing);
    let mut state = frozen_state();
    accept_verdict(&mut state, verdict);
    Fixture {
        _dir: dir,
        ctx,
        host,
        state,
    }
}

fn role(name: &str, kind: &str, pool_name: &str, read_only: bool) -> RoutedRole {
    RoutedRole {
        name: name.to_owned(),
        kind: Some(kind.to_owned()),
        status: String::from("active"),
        pool: Some(pool_name.to_owned()),
        read_only: Some(read_only),
        model: None,
        models: Vec::new(),
    }
}

fn pool() -> RoutedPool {
    RoutedPool {
        slots: 2,
        provider: String::from("local"),
        fallback: None,
        caps: std::collections::BTreeMap::new(),
        models: Vec::new(),
        has_bands: false,
    }
}

fn frozen_state() -> State {
    let mut state = State::default();
    state.needs.insert(
        NEED.to_owned(),
        NeedRecord {
            verdict: String::from("planned"),
            revision: 1,
            thread_sha256: Some(thread_hash(BODY)),
            units: vec![UNIT.to_owned()],
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        UNIT.to_owned(),
        Unit {
            need: NEED.to_owned(),
            target: String::from("default"),
            revision: 1,
            title: String::from("original unit"),
            brief: String::from("implement the need"),
            write_scope: vec![String::from("src/a.rs")],
            status: Status::Merged,
            since: 1,
            commit: Some(NEED_TIP.to_owned()),
            ..Unit::default()
        },
    );
    state.order.push(UNIT.to_owned());
    need::reserve(&mut state, "default", NEED, 1, BASE).unwrap();
    need::record_create_job(&mut state, "default", NEED, "need-branch-init").unwrap();
    need::record_created(&mut state, "default", NEED, "need-branch-init", BASE, BASE).unwrap();
    need::record_tip(&mut state, "default", NEED, 1, BASE, NEED_TIP).unwrap();
    need::freeze(&mut state, "default", NEED, 1, NEED_TIP).unwrap();
    state
}

fn accept_verdict(state: &mut State, verdict: NeedAuditVerdict) {
    let binding = need::binding_for(state, "default", NEED).unwrap().unwrap();
    let source_hash = thread_hash(BODY);
    let id = need_audit::need_audit_job_id(&binding, &source_hash, BASE, 1);
    let requirement = NeedAuditRequirement {
        id: String::from("need1234-r1"),
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
        "audit_base": BASE,
        "round": 1,
        "requirements": [requirement],
    });
    crate::pump::jobs::enqueue(state, spec).unwrap();
    let expected = need_audit::expectation_from_job(&state.jobs[&id]).unwrap();
    let receipt = NeedAuditReceipt {
        version: 1,
        receipt_key: need_audit::receipt_key(&expected),
        job_id: id.clone(),
        binding: expected.binding.clone(),
        source_hash: expected.source_hash.clone(),
        audit_base: expected.audit_base.clone(),
        round: expected.round,
        verdict,
        requirements: vec![NeedAuditRequirementResult {
            id: expected.requirements[0].id.clone(),
            text: expected.requirements[0].text.clone(),
            status: if verdict == NeedAuditVerdict::CleanPass {
                NeedRequirementStatus::Satisfied
            } else {
                NeedRequirementStatus::Unverified
            },
            evidence: vec![String::from("reviewed against the frozen range")],
        }],
        findings: Vec::new(),
        diagnoses: vec![String::from("all requested behavior is present")],
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
        "agent exit 0 in 1s\nPUMP_NEED_AUDIT_RESULT={}\n",
        serde_json::to_string(&receipt).unwrap()
    );
    let job = state.jobs.get_mut(&id).unwrap();
    job.task = Some(String::from("audit-task"));
    job.pool = Some(String::from("audit-pool"));
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent exit 0"),
        receipt: raw,
        at: 1_001,
    });
    let parsed = need_audit::parse_job_receipt(job).unwrap();
    need_verdict::set_state(
        state,
        "default",
        NEED,
        &NeedVerdictState {
            original_binding: Some(expected.binding),
            accepted: Some(AcceptedNeedReceipt {
                receipt: parsed,
                job_id: id,
            }),
            ..NeedVerdictState::default()
        },
    )
    .unwrap();
}

fn queue_landing(fixture: &mut Fixture) -> String {
    dispatch(&fixture.ctx, &mut fixture.state)
        .unwrap()
        .pop()
        .expect("accepted branch must queue")
}

fn merge_result(state: &State, job_id: &str, candidate: &str) -> String {
    let job = &state.jobs[job_id];
    let binding: NeedAuditBinding = serde_json::from_value(job.payload["binding"].clone()).unwrap();
    let source_id = job.payload["source_job_id"].as_str().unwrap();
    let checks = job.payload["required_checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| {
            serde_json::json!({
                "name": check["name"],
                "command": check["command"],
                "status": "passed",
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "version": 1,
        "target": binding.target,
        "need": binding.need,
        "revision": binding.revision,
        "base": binding.base,
        "tip": binding.tip,
        "candidate": candidate,
        "status": "merged",
        "checks": checks,
        "resolutions": [],
        "sources": [{
            "kind": "job",
            "id": source_id,
            "status": "merged",
            "why": "",
            "detail": "",
        }],
    })
    .to_string()
}

fn successful_merge_outcome(state: &State, job_id: &str) -> JobOutcome {
    let receipt = merge_result(state, job_id, CANDIDATE);
    JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent exit 0"),
        receipt: format!(
            "agent exit 0 in 1s\nPUMP_MERGE_RESULT={receipt}\nrunner publish: pushed refs/heads/next {CANDIDATE} {CANDIDATE}\nrunner contains: {NEED_TIP} 1"
        ),
        at: 1_002,
    }
}

fn finish_job(state: &mut State, id: &str, outcome: JobOutcome) {
    let job = state.jobs.get_mut(id).unwrap();
    job.status = JobStatus::Finished;
    job.outcome = Some(outcome);
}

fn thread_json(body: &str) -> String {
    serde_json::json!([{
        "handle": NEED,
        "message_id": MESSAGE_ID,
        "from": "operator",
        "body": body,
    }])
    .to_string()
}

fn reply_frozen_need_ref(host: &FakeHost) {
    host.reply(
        "ls-remote --exit-code",
        0,
        &format!("{NEED_TIP}\trefs/heads/vivi/need/{NEED}\n"),
    );
}

fn thread_hash(body: &str) -> String {
    let mut digest = Sha256::new();
    for field in [NEED, MESSAGE_ID, "operator", "", body] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    format!("v3:{}", hex::encode(digest.finalize()))
}
