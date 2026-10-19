use serde_json::json;
use std::collections::BTreeMap;
use tempfile::tempdir;

use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::need::{
    CleanupRetryState, NeedBranchPrerequisite, NeedBranchStatus, NeedCleanupReceipt,
    NeedLandingReceipt, advance_revision, all, backfill_legacy_unit_revisions, base_ref,
    begin_repair, binding_for, cleanup_retry_state, ensure_creation, external_prerequisites,
    finish_cleanup, freeze, get, managed_unlanded, need_branch, observe_branch_tip,
    quarantine_branch, record_cleanup_job, record_create_job, record_created, record_landing,
    record_tip, record_unverified_tip, release_cleanup_job, release_landing, reserve,
    reserve_landing, unit_branch, unit_branch_prefix, units_for,
};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

const BASE: &str = "1111111111111111111111111111111111111111";
const FROZEN: &str = "2222222222222222222222222222222222222222";
const LATER: &str = "3333333333333333333333333333333333333333";
const LANDING: &str = "4444444444444444444444444444444444444444";
const NEXT_TIP: &str = "5555555555555555555555555555555555555555";

fn base_state() -> State {
    let mut state = State::default();
    state.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u1")],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from("u1"),
        Unit {
            need: String::from("n1"),
            target: String::from("default"),
            revision: 1,
            status: Status::Merged,
            commit: Some(String::from(FROZEN)),
            ..Unit::default()
        },
    );
    state
}

fn frozen_state() -> State {
    let mut state = base_state();
    reserve(&mut state, "default", "n1", 1, BASE).unwrap();
    record_create_job(&mut state, "default", "n1", "init-n1").unwrap();
    record_created(&mut state, "default", "n1", "init-n1", BASE, BASE).unwrap();
    record_tip(&mut state, "default", "n1", 1, BASE, FROZEN).unwrap();
    freeze(&mut state, "default", "n1", 1, FROZEN).unwrap();
    state
}

fn merge_routing() -> RoleRouting {
    RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("merge"),
            kind: Some(String::from("merge")),
            status: String::from("active"),
            pool: Some(String::from("merge-pool")),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::from([(
            String::from("merge-pool"),
            RoutedPool {
                slots: 1,
                provider: String::from("local"),
                fallback: None,
                caps: BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    }
}

fn merge_ctx(
    dir: &std::path::Path,
    config: PumpConfig,
) -> (
    crate::pump::host::Ctx,
    std::sync::Arc<crate::pump::host::FakeHost>,
) {
    let (ctx, host) = test_ctx(dir, config);
    (ctx.with_routing(merge_routing()), host)
}

fn landed_noop_need(ctx: &crate::pump::host::Ctx) -> (State, String) {
    let mut state = State::default();
    state.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u1")],
            revision: 1,
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from("u1"),
        Unit {
            need: String::from("n1"),
            target: String::from("default"),
            revision: 1,
            status: Status::NoDefect,
            ..Unit::default()
        },
    );
    assert!(!ensure_creation(ctx, &mut state, "default", "n1", 1).unwrap());
    let created = get(&state, "default", "n1").unwrap().unwrap();
    let init_job_id = created.create_job_id.unwrap();
    record_created(&mut state, "default", "n1", &init_job_id, BASE, BASE).unwrap();
    let binding = freeze(&mut state, "default", "n1", 1, BASE).unwrap();
    finish_noop_landing(&mut state, binding);
    (state, init_job_id)
}

fn finish_noop_landing(state: &mut State, binding: crate::pump::need::NeedAuditBinding) {
    reserve_landing(state, "default", "n1", &binding, "land-n1").unwrap();
    record_landing(
        state,
        "default",
        "n1",
        "land-n1",
        NeedLandingReceipt {
            binding,
            acceptance_receipt_key: String::from("accept-noop"),
            acceptance_job_id: String::from("audit-noop"),
            verdict: String::from("clean_pass"),
            landing_job_id: String::from("land-n1"),
            target_ref: String::from("refs/heads/next"),
            candidate_sha: String::from(BASE),
            runner_published_sha: String::from(BASE),
            observed_target_sha: String::from(BASE),
            accepted_tip_in_candidate: true,
            candidate_in_observed_target: true,
            merge_report: String::from("verified no-op inclusion"),
        },
    )
    .unwrap();
    record_cleanup_job(state, "default", "n1", "delete-n1").unwrap();
    finish_cleanup(
        state,
        "default",
        "n1",
        "delete-n1",
        NeedCleanupReceipt {
            cleanup_job_id: String::from("delete-n1"),
            branch_ref: String::from("refs/heads/vivi/need/n1"),
            expected_sha: String::from(BASE),
            runner_state: String::from("deleted"),
            before_sha: Some(String::from(BASE)),
            verified_absent: true,
            completed_at: 1,
        },
    )
    .unwrap();
}

#[test]
fn namespaces_are_stable_and_scoped() {
    assert_eq!(unit_branch("u1", 3), "vivi/unit/u1-a3");
    assert_eq!(unit_branch_prefix("u1"), "vivi/unit/u1-a");
    assert_eq!(need_branch("n1"), "vivi/need/n1");
    assert_eq!(quarantine_branch("q1"), "vivi/quarantine/q1");
    assert_eq!(base_ref("origin", "vivi/need/n1"), "origin/vivi/need/n1");
}

#[test]
fn repeated_reservation_preserves_the_first_remote_base_and_survives_state_serialization() {
    let mut state = State::default();
    let first = reserve(&mut state, "default", "n1", 1, BASE).unwrap();
    let repeated = reserve(&mut state, "default", "n1", 1, FROZEN).unwrap();

    assert_eq!(first.base_sha, BASE);
    assert_eq!(repeated.base_sha, BASE);
    assert_eq!(repeated.observed_tip_sha, BASE);
    let encoded = serde_json::to_value(&state).unwrap();
    assert_eq!(encoded["need_branches"]["default"]["n1"]["base_sha"], BASE);
    let restored: State = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        get(&restored, "default", "n1").unwrap().unwrap().base_sha,
        BASE
    );
}

#[test]
fn malformed_lifecycle_state_fails_visibly_instead_of_defaulting_empty() {
    let mut state = State::default();
    state.extra.insert(
        String::from("need_branches"),
        json!({"default":{"n1":{"status":"unknown"}}}),
    );

    let error = get(&state, "default", "n1").unwrap_err().to_string();
    assert!(error.contains("need branch state"));
    assert!(all(&state).is_err());
}

#[test]
fn creation_requires_the_reserved_job_and_exact_base_tip() {
    let mut state = State::default();
    reserve(&mut state, "default", "n1", 1, BASE).unwrap();
    record_create_job(&mut state, "default", "n1", "init-n1").unwrap();

    assert!(record_created(&mut state, "default", "n1", "other", BASE, BASE).is_err());
    assert!(record_created(&mut state, "default", "n1", "init-n1", BASE, FROZEN).is_err());
    record_created(&mut state, "default", "n1", "init-n1", BASE, BASE).unwrap();
    assert_eq!(
        get(&state, "default", "n1").unwrap().unwrap().status,
        NeedBranchStatus::Active
    );
}

#[test]
fn a_frozen_binding_is_invalidated_by_a_tip_or_membership_change() {
    let mut state = frozen_state();
    assert_eq!(
        binding_for(&state, "default", "n1").unwrap().unwrap().tip,
        FROZEN
    );

    observe_branch_tip(&mut state, "default", "n1", LATER).unwrap();
    assert!(binding_for(&state, "default", "n1").unwrap().is_none());
    assert_eq!(
        get(&state, "default", "n1").unwrap().unwrap().status,
        NeedBranchStatus::Active
    );

    record_tip(&mut state, "default", "n1", 1, BASE, FROZEN).unwrap();
    freeze(&mut state, "default", "n1", 1, FROZEN).unwrap();
    state
        .needs
        .get_mut("n1")
        .unwrap()
        .units
        .push(String::from("u2"));
    state.units.insert(
        String::from("u2"),
        Unit {
            need: String::from("n1"),
            target: String::from("default"),
            revision: 1,
            status: Status::Pending,
            ..Unit::default()
        },
    );
    assert!(binding_for(&state, "default", "n1").unwrap().is_none());
    assert!(units_for(&state, "default", "n1").contains(&String::from("u2")));
}

#[test]
fn unverified_remote_tip_is_diagnostic_only_and_holds_dispatch_across_revisions() {
    let mut state = frozen_state();
    record_unverified_tip(
        &mut state,
        "default",
        "n1",
        "merge-job-1",
        LATER,
        "remote ref changed without runner ancestry proof",
    )
    .unwrap();

    let record = get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(record.status, NeedBranchStatus::Unverified);
    assert_eq!(record.branch_tip_sha.as_deref(), Some(FROZEN));
    assert_eq!(record.unverified_base_sha.as_deref(), Some(FROZEN));
    assert_eq!(record.observed_unverified_tip_sha.as_deref(), Some(LATER));
    assert_eq!(record.unverified_job_id.as_deref(), Some("merge-job-1"));
    assert!(record.frozen_tip_sha.is_none());
    assert!(binding_for(&state, "default", "n1").unwrap().is_none());
    assert!(record_tip(&mut state, "default", "n1", 1, BASE, LATER).is_err());
    assert!(observe_branch_tip(&mut state, "default", "n1", LATER).is_err());

    let advanced = advance_revision(&mut state, "default", "n1", 2).unwrap();
    assert_eq!(advanced.status, NeedBranchStatus::Unverified);
    assert_eq!(advanced.branch_tip_sha.as_deref(), Some(FROZEN));
    assert!(reserve(&mut state, "default", "n1", 2, NEXT_TIP).is_err());

    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    assert!(!ensure_creation(&ctx, &mut state, "default", "n1", 2).unwrap());
    assert_eq!(
        get(&state, "default", "n1").unwrap().unwrap().status,
        NeedBranchStatus::Unverified
    );
}

#[test]
fn a_pending_need_amendment_invalidates_the_frozen_binding() {
    let mut state = frozen_state();
    state.needs.get_mut("n1").unwrap().amendment_pending = true;

    assert!(binding_for(&state, "default", "n1").unwrap().is_none());
}

#[test]
fn repair_audit_uses_the_prior_tip_but_full_binding_keeps_the_original_base() {
    let mut state = frozen_state();
    let branch = begin_repair(&mut state, "default", "n1", 1).unwrap();
    assert_eq!(branch.audit_base_sha, FROZEN);
    assert_eq!(branch.base_sha, BASE);
    assert_eq!(branch.status, NeedBranchStatus::Active);

    record_tip(&mut state, "default", "n1", 1, BASE, LATER).unwrap();
    freeze(&mut state, "default", "n1", 1, LATER).unwrap();
    let binding = binding_for(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(binding.base, BASE);
    assert_eq!(binding.tip, LATER);
    assert_eq!(
        get(&state, "default", "n1")
            .unwrap()
            .unwrap()
            .prior_bindings
            .len(),
        1
    );
}

#[test]
fn landing_reservation_locks_revision_and_tip_until_it_is_released() {
    let mut state = frozen_state();
    let binding = binding_for(&state, "default", "n1").unwrap().unwrap();
    reserve_landing(&mut state, "default", "n1", &binding, "land-n1").unwrap();

    assert!(record_tip(&mut state, "default", "n1", 1, BASE, LATER).is_err());
    assert!(advance_revision(&mut state, "default", "n1", 2).is_err());
    release_landing(&mut state, "default", "n1", "land-n1", "pool lost").unwrap();
    record_tip(&mut state, "default", "n1", 1, BASE, LATER).unwrap();
}

#[test]
fn verified_landing_and_cleanup_are_required_for_landed_state() {
    let mut state = frozen_state();
    let binding = binding_for(&state, "default", "n1").unwrap().unwrap();
    reserve_landing(&mut state, "default", "n1", &binding, "land-n1").unwrap();
    let receipt = NeedLandingReceipt {
        binding: binding.clone(),
        acceptance_receipt_key: String::from("accept-1"),
        acceptance_job_id: String::from("audit-1"),
        verdict: String::from("clean_pass"),
        landing_job_id: String::from("land-n1"),
        target_ref: String::from("refs/heads/next"),
        candidate_sha: String::from(LANDING),
        runner_published_sha: String::from(LANDING),
        observed_target_sha: String::from(NEXT_TIP),
        accepted_tip_in_candidate: true,
        candidate_in_observed_target: true,
        merge_report: String::from("verified merge"),
    };
    record_landing(&mut state, "default", "n1", "land-n1", receipt).unwrap();
    record_cleanup_job(&mut state, "default", "n1", "delete-n1").unwrap();
    let cleanup = NeedCleanupReceipt {
        cleanup_job_id: String::from("delete-n1"),
        branch_ref: String::from("refs/heads/vivi/need/n1"),
        expected_sha: String::from(FROZEN),
        runner_state: String::from("deleted"),
        before_sha: Some(String::from(FROZEN)),
        verified_absent: true,
        completed_at: 1,
    };
    finish_cleanup(&mut state, "default", "n1", "delete-n1", cleanup).unwrap();
    assert_eq!(
        get(&state, "default", "n1").unwrap().unwrap().status,
        NeedBranchStatus::Landed
    );
    assert_eq!(
        cleanup_retry_state(&state, "default", "n1").unwrap(),
        CleanupRetryState::Complete
    );
    assert!(!managed_unlanded(&state, "n1").unwrap());
}

#[test]
fn ref_mismatch_holds_cleanup_without_an_automatic_retry() {
    let mut state = frozen_state();
    let binding = binding_for(&state, "default", "n1").unwrap().unwrap();
    reserve_landing(&mut state, "default", "n1", &binding, "land-n1").unwrap();
    record_landing(
        &mut state,
        "default",
        "n1",
        "land-n1",
        NeedLandingReceipt {
            binding,
            acceptance_receipt_key: String::from("accept-1"),
            acceptance_job_id: String::from("audit-1"),
            verdict: String::from("residual"),
            landing_job_id: String::from("land-n1"),
            target_ref: String::from("refs/heads/next"),
            candidate_sha: String::from(LANDING),
            runner_published_sha: String::from(LANDING),
            observed_target_sha: String::from(NEXT_TIP),
            accepted_tip_in_candidate: true,
            candidate_in_observed_target: true,
            merge_report: String::from("verified merge"),
        },
    )
    .unwrap();
    record_cleanup_job(&mut state, "default", "n1", "delete-n1").unwrap();
    release_cleanup_job(
        &mut state,
        "default",
        "n1",
        "delete-n1",
        "ref_mismatch",
        false,
    )
    .unwrap();

    assert_eq!(
        cleanup_retry_state(&state, "default", "n1").unwrap(),
        CleanupRetryState::Held
    );
    assert!(record_cleanup_job(&mut state, "default", "n1", "delete-n2").is_err());
    assert_eq!(
        get(&state, "default", "n1").unwrap().unwrap().status,
        NeedBranchStatus::Frozen
    );
}

#[test]
fn unknown_record_fields_survive_a_typed_update() {
    let mut state = frozen_state();
    let root = state.extra.get_mut("need_branches").unwrap();
    root["default"]["n1"]["future_field"] = json!({"retained": true});
    advance_revision(&mut state, "default", "n1", 2).unwrap();

    let record = get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(
        record.extra.get("future_field"),
        Some(&json!({"retained":true}))
    );
}

#[test]
fn legacy_stored_unit_branch_names_are_not_rewritten() {
    let mut state = base_state();
    state.units.get_mut("u1").unwrap().branch = Some(String::from("unit/u1-a1"));
    let encoded = serde_json::to_value(&state).unwrap();
    let restored: State = serde_json::from_value(encoded).unwrap();
    assert_eq!(restored.units["u1"].branch.as_deref(), Some("unit/u1-a1"));
}

#[test]
fn legacy_revision_backfill_changes_only_unrevised_units_in_the_current_plan() {
    let mut state = State::default();
    state.needs.insert(
        String::from("n1"),
        NeedRecord {
            revision: 7,
            units: vec![String::from("current")],
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from("current"),
        Unit {
            need: String::from("n1"),
            revision: 0,
            ..Unit::default()
        },
    );
    state.units.insert(
        String::from("historical"),
        Unit {
            need: String::from("n1"),
            revision: 0,
            ..Unit::default()
        },
    );
    state.units.insert(
        String::from("already-revised"),
        Unit {
            need: String::from("n1"),
            revision: 3,
            ..Unit::default()
        },
    );

    assert_eq!(backfill_legacy_unit_revisions(&mut state), 1);
    assert_eq!(state.units["current"].revision, 7);
    assert_eq!(state.units["historical"].revision, 0);
    assert_eq!(state.units["already-revised"].revision, 3);
}

#[test]
fn a_completed_legacy_replacement_tree_keeps_revision_zero() {
    let mut state = State::default();
    state.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u1")],
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from("u1"),
        Unit {
            need: String::from("n1"),
            status: Status::Replaced,
            replaced_by: vec![String::from("u1.1")],
            ..Unit::default()
        },
    );
    state.units.insert(
        String::from("u1.1"),
        Unit {
            need: String::from("n1"),
            status: Status::Merged,
            ..Unit::default()
        },
    );

    assert_eq!(backfill_legacy_unit_revisions(&mut state), 0);
    assert_eq!(state.needs["n1"].revision, 0);
    assert_eq!(state.units["u1"].revision, 0);
}

#[test]
fn a_fully_merged_legacy_need_keeps_its_legacy_revision() {
    let mut state = State::default();
    state.needs.insert(
        String::from("n1"),
        NeedRecord {
            units: vec![String::from("u1")],
            ..NeedRecord::default()
        },
    );
    state.units.insert(
        String::from("u1"),
        Unit {
            need: String::from("n1"),
            status: Status::Merged,
            ..Unit::default()
        },
    );

    assert_eq!(backfill_legacy_unit_revisions(&mut state), 0);
    assert_eq!(state.needs["n1"].revision, 0);
    assert_eq!(state.units["u1"].revision, 0);
}

#[test]
fn landed_need_can_start_a_higher_revision_after_cleanup() {
    let dir = tempdir().unwrap();
    let config = PumpConfig {
        branch: String::from("next"),
        ..PumpConfig::default()
    };
    let (ctx, host) = merge_ctx(dir.path(), config);
    host.reply(
        "git ls-remote origin refs/heads/next",
        0,
        &format!("{BASE}\trefs/heads/next\n"),
    );
    let (mut state, _) = landed_noop_need(&ctx);

    assert!(!ensure_creation(&ctx, &mut state, "default", "n1", 2).unwrap());
    let new = get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(new.status, NeedBranchStatus::Creating);
    assert_eq!(new.revision, 2);
    assert_eq!(new.base_sha, BASE);
    assert_eq!(
        new.extra["previous_branch"]["landing_receipt"]["landing_job_id"],
        "land-n1"
    );
    assert_eq!(
        new.extra["previous_branch"]["cleanup_receipt"]["verified_absent"],
        true
    );
}

#[test]
fn branch_init_job_identity_changes_with_revision_when_base_is_unchanged() {
    let dir = tempdir().unwrap();
    let config = PumpConfig {
        branch: String::from("next"),
        ..PumpConfig::default()
    };
    let (ctx, host) = merge_ctx(dir.path(), config);
    host.reply(
        "git ls-remote origin refs/heads/next",
        0,
        &format!("{BASE}\trefs/heads/next\n"),
    );
    let (mut state, first_job) = landed_noop_need(&ctx);

    assert!(!ensure_creation(&ctx, &mut state, "default", "n1", 2).unwrap());
    let second_job = get(&state, "default", "n1")
        .unwrap()
        .unwrap()
        .create_job_id
        .unwrap();
    assert_ne!(first_job, second_job);
    assert_eq!(
        get(&state, "default", "n1").unwrap().unwrap().base_sha,
        BASE
    );
}

#[test]
fn listed_units_from_an_older_revision_do_not_allow_an_empty_freeze() {
    let mut state = base_state();
    state.units.get_mut("u1").unwrap().revision = 0;
    reserve(&mut state, "default", "n1", 1, BASE).unwrap();
    record_create_job(&mut state, "default", "n1", "init-n1").unwrap();
    record_created(&mut state, "default", "n1", "init-n1", BASE, BASE).unwrap();
    record_tip(&mut state, "default", "n1", 1, BASE, FROZEN).unwrap();

    assert!(freeze(&mut state, "default", "n1", 1, FROZEN).is_err());
    assert_eq!(
        get(&state, "default", "n1").unwrap().unwrap().status,
        NeedBranchStatus::Active
    );
    assert!(binding_for(&state, "default", "n1").unwrap().is_none());
}

#[test]
fn the_branch_base_records_external_prerequisites_for_later_revision_checks() {
    let mut state = base_state();
    state.units.get_mut("u1").unwrap().after = vec![String::from("u2")];
    state.units.insert(
        String::from("u2"),
        Unit {
            need: String::from("n2"),
            target: String::from("default"),
            revision: 1,
            status: Status::Merged,
            ..Unit::default()
        },
    );
    let original = external_prerequisites(&state, "default", "n1", 1).unwrap();
    reserve(&mut state, "default", "n1", 1, BASE).unwrap();
    let record = get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(record.base_prerequisites, original);
    assert!(original.contains(&NeedBranchPrerequisite {
        unit: String::from("u2"),
        target: Some(String::from("default")),
        need: Some(String::from("n2")),
        revision: 1,
        tip: None,
    }));

    state
        .units
        .get_mut("u1")
        .unwrap()
        .after
        .push(String::from("missing"));
    let later = external_prerequisites(&state, "default", "n1", 1).unwrap();
    assert_ne!(record.base_prerequisites, later);
    assert!(later.contains(&NeedBranchPrerequisite {
        unit: String::from("missing"),
        target: None,
        need: None,
        revision: 0,
        tip: None,
    }));
}

#[test]
fn first_dispatch_persists_a_durable_merge_role_branch_init_job_and_holds_the_unit() {
    let dir = tempdir().unwrap();
    let config = PumpConfig {
        branch: String::from("next"),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), config);
    host.reply(
        "git ls-remote origin refs/heads/next",
        0,
        &format!("{BASE}\trefs/heads/next\n"),
    );
    let ctx = ctx.with_routing(RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("merge"),
            kind: Some(String::from("merge")),
            status: String::from("active"),
            pool: Some(String::from("merge-pool")),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::from([(
            String::from("merge-pool"),
            RoutedPool {
                slots: 1,
                provider: String::from("local"),
                fallback: None,
                caps: BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    });
    let mut state = base_state();
    state.units.get_mut("u1").unwrap().status = Status::Pending;

    assert!(!ensure_creation(&ctx, &mut state, "default", "n1", 1).unwrap());
    let record = get(&state, "default", "n1").unwrap().unwrap();
    let job = state
        .jobs
        .get(record.create_job_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(record.status, NeedBranchStatus::Creating);
    assert_eq!(record.base_sha, BASE);
    assert_eq!(job.kind, "need-branch-init");
    assert_eq!(job.role, "merge");
    assert_eq!(job.target.as_deref(), Some("default"));
    assert_eq!(job.from_ref.as_deref(), Some(BASE));
    assert_eq!(job.branch.as_deref(), Some("vivi/need/n1"));
    assert_eq!(job.publish_ref.as_deref(), Some("refs/heads/vivi/need/n1"));
    assert_eq!(job.publish_expect.as_deref(), Some(BASE));
    assert!(job.units.is_empty());
    assert!(job.source_jobs.is_empty());
}

#[test]
fn landed_receipts_do_not_complete_reopened_units_or_pending_amendments() {
    let dir = tempdir().unwrap();
    let config = PumpConfig {
        branch: String::from("next"),
        ..PumpConfig::default()
    };
    let (ctx, host) = merge_ctx(dir.path(), config);
    host.reply(
        "git ls-remote origin refs/heads/next",
        0,
        &format!("{BASE}\trefs/heads/next\n"),
    );
    let (mut state, _) = landed_noop_need(&ctx);
    assert!(crate::pump::need::fully_landed(&state, "n1").unwrap());
    state.units.get_mut("u1").unwrap().status = Status::Pending;
    assert!(!crate::pump::need::fully_landed(&state, "n1").unwrap());
    state.units.get_mut("u1").unwrap().status = Status::NoDefect;
    state.needs.get_mut("n1").unwrap().amendment_pending = true;
    assert!(!crate::pump::need::fully_landed(&state, "n1").unwrap());
    state.needs.get_mut("n1").unwrap().amendment_pending = false;
    state
        .needs
        .get_mut("n1")
        .unwrap()
        .units
        .push(String::from("missing-new-unit"));
    assert!(!crate::pump::need::fully_landed(&state, "n1").unwrap());
}
