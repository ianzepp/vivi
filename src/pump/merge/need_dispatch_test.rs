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
fn a_need_unit_receipt_missing_its_prefix_is_accepted_and_logged() {
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
        "agent exit 0 in 2s\nrunner source: refs/heads/vivi/unit/n1-u1-a1 {UNIT_SHA} 1\nrunner publish: pushed {branch_ref} {TIP} {TIP}\nrunner contains: {BASE} 1\n{{\"version\":1,\"target\":\"default\",\"tip\":\"{TIP}\",\"sources\":[{{\"kind\":\"unit\",\"id\":\"n1-u1\",\"status\":\"merged\",\"outside\":[],\"secs\":2}}]}}"
    );
    finish_job(&mut state, &id, result);

    super::super::pool_dispatch::apply_dispatched(&ctx, &mut state);
    assert_eq!(state.units["n1-u1"].status, Status::Merged);
    assert_eq!(state.units["n1-u1"].commit.as_deref(), Some(TIP));
    let logs = host.logs();
    assert!(
        logs.iter()
            .any(|line| line.contains("accepted without its PUMP_MERGE_RESULT= prefix")),
        "{logs:?}"
    );
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
fn a_held_need_branch_is_logged_once_and_names_the_recover_command() {
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
    need::record_unverified_tip(
        &mut state,
        "default",
        "n1",
        "job-1",
        TIP,
        "receipt rejected",
    )
    .unwrap();

    for _ in 0..3 {
        let mut waiting = BTreeMap::from([(
            String::from("default"),
            VecDeque::from([String::from("n1-u1")]),
        )]);
        let started =
            dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();
        assert!(started.is_empty());
        refresh_freezes(&ctx, &mut state).unwrap();
    }

    let held: Vec<String> = host
        .logs()
        .into_iter()
        .filter(|line| line.contains("held for explicit recovery") || line.contains("freeze held"))
        .collect();
    assert_eq!(held.len(), 1, "{held:?}");
    assert!(held[0].contains("vivi pump recover n1"), "{held:?}");
    assert!(held[0].contains(TIP), "{held:?}");
}

#[test]
fn a_recovered_need_branch_queues_its_delivered_units_for_merging_again() {
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
    need::record_unverified_tip(
        &mut state,
        "default",
        "n1",
        "job-1",
        TIP,
        "receipt rejected",
    )
    .unwrap();
    let mut waiting = BTreeMap::from([(
        String::from("default"),
        VecDeque::from([String::from("n1-u1")]),
    )]);
    let held = dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();
    assert!(held.is_empty());

    need::recover_unverified(&mut state, "default", "n1", TIP).unwrap();
    let mut waiting = BTreeMap::from([(
        String::from("default"),
        VecDeque::from([String::from("n1-u1")]),
    )]);
    let started =
        dispatch_units(&ctx, &mut state, "merge", &targets, &batches, &mut waiting).unwrap();

    let job = started
        .iter()
        .map(|id| &state.jobs[id])
        .find(|job| job.kind == "merge")
        .expect("the delivered unit is queued for merging");
    assert_eq!(job.payload["need"], "n1");
    assert_eq!(
        job.publish_contains.as_deref(),
        Some(TIP),
        "the merge starts from the adopted tip"
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

/// Cut a one-unit need merge job for `n1` and return its id, if any queued.
fn queue_need_merge(ctx: &crate::pump::host::Ctx, state: &mut State) -> Option<String> {
    let targets = ctx.cfg.target_configs();
    let batches = BTreeMap::from([(String::from("default"), 1)]);
    let mut waiting =
        BTreeMap::from([("default".to_owned(), VecDeque::from(["n1-u1".to_owned()]))]);
    let started = dispatch_units(ctx, state, "merge", &targets, &batches, &mut waiting).unwrap();
    started
        .into_iter()
        .find(|id| state.jobs[id].kind == "merge")
}

struct NeedMergeFixture {
    _dir: tempfile::TempDir,
    host: std::sync::Arc<crate::pump::host::FakeHost>,
    ctx: crate::pump::host::Ctx,
    state: State,
}

fn need_merge_fixture() -> NeedMergeFixture {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-u1"]);
    active_branch(&mut state, "n1", BASE, BASE);
    reply_unit_source(&host, "n1-u1");
    let branch_ref = "refs/heads/vivi/need/n1";
    host.reply(
        &branch_ref_command(branch_ref),
        0,
        &format!("{BASE}\t{branch_ref}\n"),
    );
    NeedMergeFixture {
        _dir: dir,
        host,
        ctx,
        state,
    }
}

impl NeedMergeFixture {
    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }

    /// Queue a merge, finish it with `sources_json` and apply it. The remote
    /// need ref still reads as the starting tip unless `published` is set.
    fn run_merge(&mut self, sources_json: &str, published: bool) -> String {
        let id = queue_need_merge(&self.ctx, &mut self.state).expect("a merge job queues");
        let branch_ref = "refs/heads/vivi/need/n1";
        let sources = sources_json.replace("{JOB}", &id);
        let proof = format!(
            "runner source: refs/heads/vivi/unit/n1-u1-a1 {UNIT_SHA} 1\nrunner publish: pushed {branch_ref} {TIP} {TIP}\nrunner contains: {BASE} 1\n"
        );
        if published {
            self.host.reply(
                &branch_ref_command(branch_ref),
                0,
                &format!("{TIP}\t{branch_ref}\n"),
            );
        }
        let receipt = format!(
            "agent exit 0 in 2s\n{proof}PUMP_MERGE_RESULT={{\"version\":1,\"target\":\"default\",\"tip\":\"{TIP}\",\"shared_check_timeouts\":0,\"shared_check_timeout_retries\":0,\"shared_check_recoveries\":[],\"sources\":{sources}}}"
        );
        finish_job(&mut self.state, &id, receipt);
        super::super::pool_dispatch::apply_dispatched(&self.ctx, &mut self.state);
        id
    }
}

const OWN_JOB_SOURCES: &str = r#"[{"kind":"unit","id":"n1-u1","status":"merged","why":"","detail":"merged","outside":[],"secs":0},{"kind":"job","id":"{JOB}","status":"merged","why":"","detail":"merged","outside":[],"secs":0}]"#;
const STAGE_SOURCES: &str = r#"[{"kind":"unit","id":"n1-u1","status":"undecided","why":"shared-check-environment","detail":"stage 1 failed: env: python3.11 not found","outside":[],"secs":45}]"#;
const BAD_SOURCES: &str = r#"[{"kind":"unit","id":"n1-u1","status":"merged","why":"","detail":"","outside":[],"secs":0},{"kind":"job","id":"some-other-job","status":"merged","why":"","detail":"","outside":[],"secs":0}]"#;

#[test]
fn a_job_entry_naming_the_running_job_itself_is_ignored() {
    let mut fx = need_merge_fixture();
    let id = fx.run_merge(OWN_JOB_SOURCES, true);

    assert!(fx.state.jobs[&id].applied_at.is_some());
    assert_eq!(fx.state.units["n1-u1"].status, Status::Merged);
    assert_eq!(
        need::get(&fx.state, "default", "n1")
            .unwrap()
            .unwrap()
            .branch_tip_sha
            .as_deref(),
        Some(TIP)
    );
    assert!(!fx.exceptions().contains("need-merge-unverified"));
}

#[test]
fn a_job_entry_naming_any_other_job_still_fails_validation_with_its_reason() {
    let mut fx = need_merge_fixture();
    let id = fx.run_merge(BAD_SOURCES, false);

    assert!(fx.state.jobs[&id].applied_at.is_some());
    assert_eq!(fx.state.units["n1-u1"].status, Status::Delivered);
    let exceptions = fx.exceptions();
    assert!(exceptions.contains("need-merge-unverified"), "{exceptions}");
    assert!(
        exceptions.contains("job source `some-other-job` was not requested"),
        "{exceptions}"
    );
}

#[test]
fn an_undecided_unit_with_a_why_code_is_a_valid_receipt_and_returns_the_unit() {
    let mut fx = need_merge_fixture();
    let id = fx.run_merge(STAGE_SOURCES, true);

    assert!(fx.state.jobs[&id].applied_at.is_some());
    assert_eq!(fx.state.units["n1-u1"].status, Status::Delivered);
    let exceptions = fx.exceptions();
    assert!(!exceptions.contains("need-merge"), "{exceptions}");
    assert!(
        fx.host
            .logs()
            .iter()
            .any(|line| line.starts_with("shared-check-environment n1-u1")),
        "undecided path should log the why code"
    );
}

#[test]
fn the_third_consecutive_invalid_receipt_raises_one_exception_and_stops_requeuing() {
    let mut fx = need_merge_fixture();
    let first = fx.run_merge(BAD_SOURCES, false);
    let second = fx.run_merge(BAD_SOURCES, false);
    assert!(!fx.exceptions().contains("need-merge-receipt-invalid"));
    let third = fx.run_merge(BAD_SOURCES, false);

    let exceptions = fx.exceptions();
    assert_eq!(exceptions.matches("need-merge-receipt-invalid").count(), 1);
    for id in [&first, &second, &third] {
        assert!(exceptions.contains(id.as_str()), "{exceptions}");
    }
    assert!(exceptions.contains("was not requested"), "{exceptions}");
    assert_eq!(fx.state.units["n1-u1"].status, Status::Delivered);

    assert!(queue_need_merge(&fx.ctx, &mut fx.state).is_none());

    fx.state.needs.get_mut("n1").unwrap().revision = 2;
    fx.state.units.get_mut("n1-u1").unwrap().revision = 2;
    assert!(queue_need_merge(&fx.ctx, &mut fx.state).is_some());
}

#[test]
fn a_valid_receipt_ends_the_invalid_receipt_streak() {
    let mut fx = need_merge_fixture();
    fx.run_merge(BAD_SOURCES, false);
    fx.run_merge(BAD_SOURCES, false);
    fx.run_merge(STAGE_SOURCES, true);
    let branch = need::get(&fx.state, "default", "n1").unwrap().unwrap();
    assert!(branch.invalid_receipts.is_empty());
}

#[test]
fn the_need_merge_brief_names_no_job_entry_for_its_own_job() {
    let mut fx = need_merge_fixture();
    let id = queue_need_merge(&fx.ctx, &mut fx.state).unwrap();
    let body = &fx.state.jobs[&id].body;
    assert!(body.contains("This job has no role sources"));
    assert!(body.contains("never name this job's own id"));
    assert!(!body.contains("\"secs\":0},{\"kind\":\"job\""));
}

#[test]
fn an_unchanged_stuck_disposition_is_logged_once() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut state = State::default();
    add_need(&mut state, "n1", &["n1-stuck"]);
    active_branch(&mut state, "n1", BASE, BASE);
    state.units.insert(
        String::from("n1-stuck"),
        unit("n1", Status::Stuck, 1, Some("unit/n1-stuck-a1")),
    );

    refresh_freezes(&ctx, &mut state).unwrap();
    refresh_freezes(&ctx, &mut state).unwrap();
    let recorded = |host: &crate::pump::host::FakeHost| {
        host.logs()
            .iter()
            .filter(|line| line.starts_with("need unit disposition recorded"))
            .count()
    };
    assert_eq!(recorded(&host), 1);

    state
        .units
        .get_mut("n1-stuck")
        .unwrap()
        .history
        .push(crate::pump::state::Attempt {
            detail: String::from("proof failed again"),
            ..Default::default()
        });
    refresh_freezes(&ctx, &mut state).unwrap();
    assert_eq!(recorded(&host), 2);
}

struct CapFixture {
    _dir: tempfile::TempDir,
    host: std::sync::Arc<crate::pump::host::FakeHost>,
    ctx: crate::pump::host::Ctx,
    state: State,
}

/// `count` needs `n1..nN`, each with one delivered unit on an active branch.
/// `cap` overrides `[pump.merge] need_parallel` when given.
fn cap_fixture(count: usize, cap: Option<u32>) -> CapFixture {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    if let Some(cap) = cap {
        config.merge.need_parallel = cap;
    }
    let (base, host) = test_ctx(dir.path(), config);
    let ctx = base.with_routing(routing());
    let mut state = State::default();
    for index in 1..=count {
        let need_id = format!("n{index}");
        let uid = format!("{need_id}-u1");
        add_need(&mut state, &need_id, &[uid.as_str()]);
        active_branch(&mut state, &need_id, BASE, BASE);
        reply_unit_source(&host, &uid);
    }
    CapFixture {
        _dir: dir,
        host,
        ctx,
        state,
    }
}

impl CapFixture {
    /// One dispatch tick over every unit still `Delivered`, in unit-id order.
    fn tick(&mut self) -> Vec<String> {
        let targets = self.ctx.cfg.target_configs();
        let batches = BTreeMap::from([(String::from("default"), 1)]);
        let queue: VecDeque<String> = self
            .state
            .units
            .iter()
            .filter(|(_, unit)| unit.status == Status::Delivered)
            .map(|(uid, _)| uid.clone())
            .collect();
        let mut waiting = BTreeMap::from([(String::from("default"), queue)]);
        dispatch_units(
            &self.ctx,
            &mut self.state,
            "merge",
            &targets,
            &batches,
            &mut waiting,
        )
        .unwrap()
    }

    /// The needs that own the given started merge jobs, sorted.
    fn needs_of(&self, started: &[String]) -> Vec<String> {
        let mut needs: Vec<String> = started
            .iter()
            .filter_map(|id| self.state.jobs[id].payload["need"].as_str())
            .map(str::to_owned)
            .collect();
        needs.sort();
        needs
    }

    fn cap_lines(&self) -> Vec<String> {
        self.host
            .logs()
            .into_iter()
            .filter(|line| line.contains("need-merge cap"))
            .collect()
    }
}

#[test]
fn need_merge_cap_defaults_to_four_per_target() {
    assert_eq!(crate::pump::config::MergeConfig::default().need_parallel, 4);
    let mut fx = cap_fixture(6, None);

    let started = fx.tick();

    assert_eq!(started.len(), 4, "{started:?}");
    assert_eq!(
        fx.cap_lines(),
        ["need-merge cap default: 4/4 active, 2 waiting"]
    );
}

#[test]
fn need_merge_cap_holds_across_needs_and_logs_once_per_tick() {
    let mut fx = cap_fixture(5, Some(2));

    let started = fx.tick();

    assert_eq!(started.len(), 2, "{started:?}");
    assert_eq!(
        fx.cap_lines(),
        ["need-merge cap default: 2/2 active, 3 waiting"]
    );
    let merging = fx
        .state
        .units
        .values()
        .filter(|unit| unit.status == Status::Merging)
        .count();
    assert_eq!(merging, 2, "deferred units stay delivered");

    // Still full on the next tick: nothing more starts, one more log line.
    assert!(fx.tick().is_empty());
    assert_eq!(fx.cap_lines().len(), 2);
}

#[test]
fn need_merge_cap_starts_priority_needs_first() {
    let mut fx = cap_fixture(4, Some(2));
    crate::pump::priority::set(&fx.ctx.home, "n4", 5).unwrap();
    crate::pump::priority::set(&fx.ctx.home, "n3", 1).unwrap();

    let started = fx.tick();

    assert_eq!(fx.needs_of(&started), ["n3", "n4"]);
}

#[test]
fn need_merge_cap_starts_fix_units_before_priority_needs() {
    let mut fx = cap_fixture(3, Some(1));
    let fix_uid = format!("{}n1", fx.ctx.cfg.fix_prefix);
    let fix = fx.state.units.remove("n1-u1").unwrap();
    fx.state.units.insert(fix_uid.clone(), fix);
    fx.state.units.get_mut(&fix_uid).unwrap().branch = Some(format!("vivi/unit/{fix_uid}-a1"));
    fx.state.needs.get_mut("n1").unwrap().units = vec![fix_uid.clone()];
    reply_unit_source(&fx.host, &fix_uid);
    crate::pump::priority::set(&fx.ctx.home, "n3", 9).unwrap();

    let started = fx.tick();

    assert_eq!(fx.needs_of(&started), ["n1"]);
}

#[test]
fn need_merge_cap_frees_a_slot_for_the_next_need_on_a_later_tick() {
    let mut fx = cap_fixture(3, Some(2));
    let first = fx.tick();
    assert_eq!(first.len(), 2);
    assert!(fx.tick().is_empty());

    // A finished job stops running its checks and frees its slot, even
    // before its receipt is applied.
    fx.state.jobs.get_mut(&first[0]).unwrap().status = JobStatus::Finished;
    let next = fx.tick();

    assert_eq!(next.len(), 1, "{next:?}");
    let mut all = first.clone();
    all.extend(next);
    assert_eq!(fx.needs_of(&all), ["n1", "n2", "n3"]);
}

#[test]
fn need_merge_cap_counts_only_need_merge_jobs() {
    let mut fx = cap_fixture(2, Some(1));
    for (id, kind) in [("init-x", "need-branch-init"), ("shared-1", "merge")] {
        let mut spec = crate::pump::jobs::JobSpec::new(id, kind, "merge", "s", "b");
        spec.target = Some(String::from("default"));
        crate::pump::jobs::enqueue(&mut fx.state, spec).unwrap();
    }

    let started = fx.tick();

    assert_eq!(fx.needs_of(&started).len(), 1, "{started:?}");
}
