use std::collections::{BTreeMap, VecDeque};

use super::{dispatch_units, refresh_freezes};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobStatus};
use crate::pump::need::{self, NeedBranchStatus};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

const BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE_2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TIP: &str = "cccccccccccccccccccccccccccccccccccccccc";
const UNIT_SHA: &str = "dddddddddddddddddddddddddddddddddddddddd";

#[test]
fn unit_jobs_are_isolated_by_need_and_keep_legacy_units_out_of_the_new_route() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 2)]);
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-u1", "n1-u2"]);
    add_need(&mut state, "n2", &["n2-u1"]);
    active_branch(&mut state, "n1", BASE, BASE_2);
    active_branch(&mut state, "n2", BASE, TIP);
    reply_unit_sources(&host, &["n1-u1", "n1-u2"]);
    reply_unit_source(&host, "n2-u1");
    let mut legacy = unit("", Status::Delivered, 1, Some("unit/legacy-u1-a1"));
    legacy.need.clear();
    state.units.insert(String::from("legacy-u1"), legacy);
    let mut waiting = BTreeMap::from([(
        String::from("default"),
        VecDeque::from([
            String::from("n1-u1"),
            String::from("n2-u1"),
            String::from("n1-u2"),
            String::from("legacy-u1"),
        ]),
    )]);

    let started =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();

    assert_eq!(started.len(), 2);
    assert_eq!(
        waiting["default"],
        VecDeque::from([String::from("legacy-u1")])
    );
    let need_jobs: Vec<_> = state
        .jobs
        .values()
        .filter(|job| {
            job.payload
                .get("need_merge")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        })
        .collect();
    assert_eq!(need_jobs.len(), 2);
    for job in need_jobs {
        let need_id = job.payload["need"].as_str().unwrap();
        let expected_branch = format!("refs/heads/vivi/need/{need_id}");
        assert_eq!(job.target.as_deref(), Some("default"));
        assert_eq!(job.publish_ref.as_deref(), Some(expected_branch.as_str()));
        assert!(job.source_jobs.is_empty());
        let sources = job.payload["unit_sources"].as_array().unwrap();
        let source_ids: Vec<&str> = sources
            .iter()
            .map(|source| source["id"].as_str().unwrap())
            .collect();
        assert!(
            source_ids
                .iter()
                .all(|uid| state.units[*uid].need == need_id)
        );
        assert!(!source_ids.iter().any(|uid| uid.starts_with("legacy")));
        assert!(sources.iter().all(|source| {
            source["source_ref"]
                .as_str()
                .is_some_and(|reference| reference.starts_with("refs/heads/vivi/unit/"))
                && source["source_sha"].as_str() == Some(UNIT_SHA)
        }));
    }
    assert!(
        state
            .jobs
            .values()
            .all(|job| job.publish_ref.as_deref() != Some("refs/heads/next"))
    );
}

#[test]
fn local_pool_unit_source_is_pinned_from_the_shared_target_repo() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let local_repo = base.for_target("default").unwrap().repo;
    let ctx = base
        .with_routing(routing())
        .with_pool_sources(BTreeMap::from([(String::from("local"), local_repo)]));
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-u1"]);
    active_branch(&mut state, "n1", BASE, BASE);
    state.units.get_mut("n1-u1").unwrap().pool = Some(String::from("local"));
    host.reply(
        "for-each-ref --format",
        0,
        &format!("{UNIT_SHA}\trefs/heads/vivi/unit/n1-u1-a1\tcommit\n"),
    );
    let mut waiting =
        BTreeMap::from([("default".to_owned(), VecDeque::from(["n1-u1".to_owned()]))]);

    let started =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();

    assert_eq!(started.len(), 1);
    let job = &state.jobs[&started[0]];
    assert_eq!(job.payload["unit_sources"][0]["source_sha"], UNIT_SHA);
    assert!(
        host.commands()
            .iter()
            .any(|command| command.contains("for-each-ref --format"))
    );
    assert!(
        host.commands()
            .iter()
            .all(|command| !command.contains("ls-remote origin refs/heads/vivi/unit"))
    );
}

#[test]
fn multi_target_local_resolution_matches_pool_target_source_mode() {
    let worktree_pool: crate::pool::config::PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 1\n[local]\nworktree_of = \"/shared/target\"\n",
    )
    .unwrap();
    let github_pool: crate::pool::config::PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 1\n[github]\napp_id = 1\nprivate_key = \"unused\"\n",
    )
    .unwrap();

    assert!(super::pool_uses_target_repo(&worktree_pool));
    assert!(!super::pool_uses_target_repo(&github_pool));
}

#[test]
fn a_unit_with_missing_need_plan_cannot_fall_back_to_the_shared_target() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    active_branch(&mut state, "missing-need", BASE, BASE);
    state.units.insert(
        String::from("orphan-u1"),
        unit(
            "missing-need",
            Status::Delivered,
            1,
            Some("vivi/unit/orphan-u1-a1"),
        ),
    );
    let mut waiting = BTreeMap::from([(
        String::from("default"),
        VecDeque::from([String::from("orphan-u1")]),
    )]);

    let started =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();

    assert!(started.is_empty());
    assert!(waiting["default"].is_empty());
    assert_eq!(state.units["orphan-u1"].status, Status::NeedsReplan);
    assert!(
        state
            .jobs
            .values()
            .all(|job| job.publish_ref.as_deref() != Some("refs/heads/next"))
    );
}

#[test]
fn init_receipt_activates_only_the_exact_need_ref_and_replays_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-u1"]);
    state
        .units
        .insert(String::from("n1-u1"), unit("n1", Status::Pending, 1, None));
    host.reply(
        "ls-remote origin refs/heads/next",
        0,
        &format!("{BASE}\trefs/heads/next\n"),
    );
    assert!(!need::ensure_creation(&ctx, &mut state, "default", "n1", 1).unwrap());
    let job_id = state.jobs.keys().next().unwrap().clone();
    let branch_ref = "refs/heads/vivi/need/n1";
    host.reply(
        &branch_ref_command(branch_ref),
        0,
        &format!("{BASE}\t{branch_ref}\n"),
    );
    let receipt = format!(
        "agent exit 0 in 2s\nrunner publish: pushed {branch_ref} {BASE} {BASE}\nPUMP_NEED_BRANCH_RESULT={{\"version\":1,\"status\":\"ready\",\"target\":\"default\",\"need\":\"n1\",\"branch\":\"vivi/need/n1\",\"base_sha\":\"{BASE}\",\"tip_sha\":\"{BASE}\"}}"
    );
    finish_job(&mut state, &job_id, receipt);

    super::super::pool_dispatch::apply_dispatched(&ctx, &mut state);
    let branch = need::get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(branch.status, NeedBranchStatus::Active);
    assert_eq!(branch.branch_tip_sha.as_deref(), Some(BASE));
    assert!(state.jobs[&job_id].applied_at.is_some());
    let queries = host
        .commands()
        .iter()
        .filter(|command| command.contains(&format!("ls-remote origin {branch_ref}")))
        .count();
    super::super::pool_dispatch::apply_dispatched(&ctx, &mut state);
    let repeated = host
        .commands()
        .iter()
        .filter(|command| command.contains(&format!("ls-remote origin {branch_ref}")))
        .count();
    assert_eq!(repeated, queries);
}

#[test]
fn all_no_defect_need_initializes_and_freezes_an_empty_branch_range() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    add_need(&mut state, "no-defect", &["no-defect-u1"]);
    state.units.get_mut("no-defect-u1").unwrap().status = Status::NoDefect;
    host.reply(
        "ls-remote origin refs/heads/next",
        0,
        &format!("{BASE}\trefs/heads/next\n"),
    );

    let started = dispatch_units(
        &ctx,
        &mut state,
        "merge",
        &targets,
        &batches,
        &mut BTreeMap::new(),
    )
    .unwrap();

    assert_eq!(started.len(), 1);
    let job_id = started[0].clone();
    assert_eq!(state.jobs[&job_id].kind, "need-branch-init");
    let branch_ref = "refs/heads/vivi/need/no-defect";
    host.reply(
        &branch_ref_command(branch_ref),
        0,
        &format!("{BASE}\t{branch_ref}\n"),
    );
    let receipt = format!(
        "agent exit 0 in 2s\nrunner publish: pushed {branch_ref} {BASE} {BASE}\nPUMP_NEED_BRANCH_RESULT={{\"version\":1,\"status\":\"ready\",\"target\":\"default\",\"need\":\"no-defect\",\"branch\":\"vivi/need/no-defect\",\"base_sha\":\"{BASE}\",\"tip_sha\":\"{BASE}\"}}"
    );
    finish_job(&mut state, &job_id, receipt);
    super::super::pool_dispatch::apply_dispatched(&ctx, &mut state);

    let branch = need::get(&state, "default", "no-defect").unwrap().unwrap();
    assert_eq!(branch.status, NeedBranchStatus::Frozen);
    assert_eq!(branch.frozen_tip_sha.as_deref(), Some(BASE));
    assert_eq!(state.merges, 0);
}

#[test]
fn a_failed_ls_remote_for_one_need_does_not_stop_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    add_need(&mut state, "a-settled", &["a-settled-u1"]);
    state.units.get_mut("a-settled-u1").unwrap().status = Status::NoDefect;
    add_need(&mut state, "b-active", &["b-active-u1"]);
    active_branch(&mut state, "b-active", BASE, BASE);
    reply_unit_source(&host, "b-active-u1");
    host.reply(
        "ls-remote origin refs/heads/next",
        128,
        "fatal: unable to access the remote",
    );
    let mut waiting = BTreeMap::from([(
        String::from("default"),
        VecDeque::from([String::from("b-active-u1")]),
    )]);

    let started =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();

    assert_eq!(started.len(), 1, "the healthy need still queues its merge");
    assert_eq!(state.jobs[&started[0]].payload["need"], "b-active");
    assert!(need::get(&state, "default", "a-settled").unwrap().is_none());
    let raised = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        raised.contains("tick-step-failed seed-need default/a-settled"),
        "{raised}"
    );
    assert!(
        raised.contains("cannot observe target integration tip"),
        "{raised}"
    );
}

#[test]
fn merged_need_units_freeze_exactly_without_counting_as_next_merges() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-u1"]);
    active_branch(&mut state, "n1", BASE, BASE);
    reply_unit_source(&host, "n1-u1");
    state.units.insert(
        String::from("n1-u1"),
        unit("n1", Status::Delivered, 1, Some("vivi/unit/n1-u1-a1")),
    );
    let mut waiting =
        BTreeMap::from([("default".to_owned(), VecDeque::from(["n1-u1".to_owned()]))]);
    let started =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();
    let id = started
        .iter()
        .find(|id| state.jobs[*id].kind == "merge")
        .unwrap()
        .clone();
    let branch_ref = "refs/heads/vivi/need/n1";
    host.reply(
        &branch_ref_command(branch_ref),
        0,
        &format!("{TIP}\t{branch_ref}\n"),
    );
    let result = format!(
        "agent exit 0 in 2s\nrunner source: refs/heads/vivi/unit/n1-u1-a1 {UNIT_SHA} 1\nrunner publish: pushed {branch_ref} {TIP} {TIP}\nrunner contains: {BASE} 1\nPUMP_MERGE_RESULT={{\"version\":1,\"target\":\"default\",\"tip\":\"{TIP}\",\"sources\":[{{\"kind\":\"unit\",\"id\":\"n1-u1\",\"status\":\"merged\",\"outside\":[],\"secs\":2}}]}}"
    );
    finish_job(&mut state, &id, result);

    super::super::pool_dispatch::apply_dispatched(&ctx, &mut state);
    assert_eq!(state.units["n1-u1"].status, Status::Merged);
    assert_eq!(state.units["n1-u1"].commit.as_deref(), Some(TIP));
    assert_eq!(state.merges, 0);
    assert!(state.closed_needs.is_empty());
    assert!(!ctx.home.join("needs-ready.txt").exists());
    assert!(state.jobs[&id].applied_at.is_some());
    let branch = need::get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(branch.status, NeedBranchStatus::Frozen);
    assert_eq!(branch.frozen_tip_sha.as_deref(), Some(TIP));
    let binding = need::binding_for(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(binding.revision, 1);
    assert_eq!(binding.base, BASE);
    assert_eq!(binding.tip, TIP);
    super::super::pool_dispatch::apply_dispatched(&ctx, &mut state);
    assert_eq!(state.units["n1-u1"].merged_at, Some(ctx.now()));
}

#[test]
fn need_unit_merge_without_runner_source_ancestry_is_not_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-u1"]);
    active_branch(&mut state, "n1", BASE, BASE);
    reply_unit_source(&host, "n1-u1");
    let mut waiting =
        BTreeMap::from([("default".to_owned(), VecDeque::from(["n1-u1".to_owned()]))]);
    let started =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();
    let id = started
        .iter()
        .find(|id| state.jobs[*id].kind == "merge")
        .unwrap()
        .clone();
    let branch_ref = "refs/heads/vivi/need/n1";
    host.reply(
        &branch_ref_command(branch_ref),
        0,
        &format!("{TIP}\t{branch_ref}\n"),
    );
    let result = format!(
        "agent exit 0 in 2s\nrunner publish: pushed {branch_ref} {TIP} {TIP}\nPUMP_MERGE_RESULT={{\"version\":1,\"target\":\"default\",\"tip\":\"{TIP}\",\"sources\":[{{\"kind\":\"unit\",\"id\":\"n1-u1\",\"status\":\"merged\",\"outside\":[],\"secs\":2}}]}}"
    );
    finish_job(&mut state, &id, result);

    super::super::pool_dispatch::apply_dispatched(&ctx, &mut state);

    let branch = need::get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(state.units["n1-u1"].status, Status::Delivered);
    assert_eq!(branch.branch_tip_sha.as_deref(), Some(BASE));
    assert_eq!(branch.unverified_base_sha.as_deref(), Some(BASE));
    assert_eq!(branch.observed_unverified_tip_sha.as_deref(), Some(TIP));
    assert_eq!(branch.unverified_job_id.as_deref(), Some(id.as_str()));
    assert_eq!(branch.status, NeedBranchStatus::Unverified);
    assert!(branch.frozen_tip_sha.is_none());

    state.needs.get_mut("n1").unwrap().revision = 2;
    state.units.get_mut("n1-u1").unwrap().revision = 2;
    let mut waiting =
        BTreeMap::from([("default".to_owned(), VecDeque::from(["n1-u1".to_owned()]))]);
    let retried =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();

    assert!(retried.is_empty());
    assert_eq!(state.units["n1-u1"].status, Status::Delivered);
    assert_eq!(
        need::get(&state, "default", "n1").unwrap().unwrap().status,
        NeedBranchStatus::Unverified
    );
}

#[test]
fn current_stuck_and_replaced_units_record_blockers_but_do_not_freeze() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _) = test_ctx(dir.path(), PumpConfig::default());
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-stuck", "n1-replaced"]);
    active_branch(&mut state, "n1", BASE, BASE);
    let mut stuck = unit("n1", Status::Stuck, 1, Some("unit/n1-stuck-a1"));
    stuck.history.push(crate::pump::state::Attempt {
        detail: String::from("proof failed"),
        ..Default::default()
    });
    state.units.insert(String::from("n1-stuck"), stuck);
    let mut replaced = unit("n1", Status::Replaced, 1, Some("unit/n1-replaced-a1"));
    replaced.replaced_by = vec![String::from("n1-new")];
    state.units.insert(String::from("n1-replaced"), replaced);

    refresh_freezes(&ctx, &mut state).unwrap();

    let branch = need::get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(branch.status, NeedBranchStatus::Active);
    assert!(branch.frozen_tip_sha.is_none());
    assert_eq!(branch.unit_dispositions["n1-stuck"].kind, "stuck");
    assert_eq!(branch.unit_dispositions["n1-replaced"].kind, "replaced");
    assert!(!need::current_units_settled(&state, "default", "n1", 1).unwrap());
}

#[test]
fn first_need_branch_waits_until_all_external_unit_prerequisites_are_landed() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, _) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    add_need(&mut state, "prereq", &["prereq-u1"]);
    active_branch(&mut state, "prereq", BASE, BASE);
    state.units.get_mut("prereq-u1").unwrap().status = Status::Merged;
    add_need(&mut state, "consumer", &["consumer-u1"]);
    let consumer = state.units.get_mut("consumer-u1").unwrap();
    consumer.branch = Some(String::from("unit/consumer-u1-a1"));
    consumer.after = vec![String::from("prereq-u1")];
    let mut waiting = BTreeMap::from([(
        String::from("default"),
        VecDeque::from([String::from("consumer-u1")]),
    )]);

    dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();

    assert!(need::get(&state, "default", "consumer").unwrap().is_none());
    assert!(state.jobs.is_empty());
    assert_eq!(state.units["consumer-u1"].status, Status::Delivered);
}

#[test]
fn a_new_external_prerequisite_after_branch_base_requests_replanning() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, _) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    add_need(&mut state, "prereq", &["prereq-u1"]);
    active_branch(&mut state, "prereq", BASE, BASE);
    state.units.get_mut("prereq-u1").unwrap().status = Status::Merged;
    add_need(&mut state, "consumer", &["consumer-u1"]);
    active_branch(&mut state, "consumer", BASE, BASE);
    let consumer = state.units.get_mut("consumer-u1").unwrap();
    consumer.branch = Some(String::from("unit/consumer-u1-a1"));
    consumer.after = vec![String::from("prereq-u1")];
    let mut waiting = BTreeMap::from([(
        String::from("default"),
        VecDeque::from([String::from("consumer-u1")]),
    )]);

    dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();

    assert_eq!(state.units["consumer-u1"].status, Status::NeedsReplan);
    assert!(state.needs["consumer"].amendment_pending);
    assert!(
        state
            .jobs
            .values()
            .all(|job| job.payload.get("need_merge").is_none())
    );
}

#[test]
fn late_old_revision_tip_advances_branch_truth_without_completing_current_units() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-u1"]);
    active_branch(&mut state, "n1", BASE, BASE);
    reply_unit_source(&host, "n1-u1");
    state.units.insert(
        String::from("n1-u1"),
        unit("n1", Status::Delivered, 1, Some("vivi/unit/n1-u1-a1")),
    );
    let mut waiting =
        BTreeMap::from([("default".to_owned(), VecDeque::from(["n1-u1".to_owned()]))]);
    let started =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();
    let id = started
        .iter()
        .find(|id| state.jobs[*id].kind == "merge")
        .unwrap()
        .clone();
    state.needs.get_mut("n1").unwrap().revision = 2;
    state.units.get_mut("n1-u1").unwrap().revision = 2;
    state.units.get_mut("n1-u1").unwrap().status = Status::Pending;
    need::advance_revision(&mut state, "default", "n1", 2).unwrap();
    let branch_ref = "refs/heads/vivi/need/n1";
    host.reply(
        &branch_ref_command(branch_ref),
        0,
        &format!("{TIP}\t{branch_ref}\n"),
    );
    let result = format!(
        "agent exit 0 in 2s\nrunner source: refs/heads/vivi/unit/n1-u1-a1 {UNIT_SHA} 1\nrunner publish: pushed {branch_ref} {TIP} {TIP}\nrunner contains: {BASE} 1\nPUMP_MERGE_RESULT={{\"version\":1,\"target\":\"default\",\"tip\":\"{TIP}\",\"sources\":[{{\"kind\":\"unit\",\"id\":\"n1-u1\",\"status\":\"merged\",\"outside\":[],\"secs\":2}}]}}"
    );
    finish_job(&mut state, &id, result);

    super::super::pool_dispatch::apply_dispatched(&ctx, &mut state);

    let branch = need::get(&state, "default", "n1").unwrap().unwrap();
    assert_eq!(branch.branch_tip_sha.as_deref(), Some(TIP));
    assert_eq!(branch.status, NeedBranchStatus::Active);
    assert!(branch.frozen_tip_sha.is_none());
    assert_eq!(state.units["n1-u1"].status, Status::Pending);
    assert!(state.jobs[&id].applied_at.is_some());
}

#[test]
fn closeout_requires_landing_for_new_revision_units_but_preserves_legacy_state() {
    let mut current = State::default();
    add_need(&mut current, "new-need", &["new-u1"]);
    current.units.get_mut("new-u1").unwrap().status = Status::Merged;
    assert!(crate::pump::report::landing_required(&current, "new-need"));
    assert!(!need::fully_landed(&current, "new-need").unwrap());

    let mut legacy = State::default();
    legacy.needs.insert(
        String::from("old-need"),
        NeedRecord {
            verdict: String::from("units"),
            revision: 1,
            units: vec![String::from("old-u1")],
            ..NeedRecord::default()
        },
    );
    legacy.units.insert(
        String::from("old-u1"),
        unit("old-need", Status::Merged, 0, Some("unit/old-u1-a1")),
    );
    assert!(!crate::pump::report::landing_required(&legacy, "old-need"));
}

fn active_branch(st: &mut State, need_id: &str, base: &str, tip: &str) {
    need::reserve(st, "default", need_id, 1, base).unwrap();
    need::record_create_job(st, "default", need_id, &format!("init-{need_id}")).unwrap();
    need::record_created(
        st,
        "default",
        need_id,
        &format!("init-{need_id}"),
        base,
        base,
    )
    .unwrap();
    if tip != base {
        need::record_tip(st, "default", need_id, 1, base, tip).unwrap();
    }
}

fn add_need(st: &mut State, need_id: &str, units: &[&str]) {
    st.needs.insert(
        need_id.to_owned(),
        NeedRecord {
            verdict: String::from("units"),
            revision: 1,
            units: units.iter().map(|unit| unit.to_string()).collect(),
            ..NeedRecord::default()
        },
    );
    for uid in units {
        let mut unit = unit(need_id, Status::Delivered, 1, None);
        unit.branch = Some(format!("vivi/unit/{uid}-a1"));
        st.units.insert(uid.to_string(), unit);
    }
}

fn unit(need_id: &str, status: Status, revision: u64, branch: Option<&str>) -> Unit {
    Unit {
        need: need_id.to_owned(),
        target: String::from("default"),
        revision,
        branch: branch.map(str::to_owned),
        status,
        ..Unit::default()
    }
}

fn finish_job(st: &mut State, id: &str, receipt: String) {
    let job = st.jobs.get_mut(id).unwrap();
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("done"),
        receipt,
        at: 1,
    });
}

fn branch_ref_command(reference: &str) -> String {
    format!("ls-remote origin {reference}")
}

fn reply_unit_source(host: &crate::pump::host::FakeHost, unit: &str) {
    reply_unit_sources(host, &[unit]);
}

fn reply_unit_sources(host: &crate::pump::host::FakeHost, units: &[&str]) {
    let references: Vec<String> = units
        .iter()
        .map(|unit| format!("refs/heads/vivi/unit/{unit}-a1"))
        .collect();
    let query = references.join(" ");
    let output = references
        .iter()
        .map(|reference| format!("{UNIT_SHA}\t{reference}"))
        .collect::<Vec<_>>()
        .join("\n");
    host.reply(&format!("ls-remote origin {query}"), 0, &output);
}

fn routing() -> RoleRouting {
    RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("merge"),
            kind: Some(String::from("merge")),
            status: String::from("active"),
            pool: Some(String::from("local")),
            read_only: Some(false),
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::from([(
            String::from("local"),
            RoutedPool {
                slots: 2,
                provider: String::from("local"),
                fallback: None,
                caps: BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    }
}
