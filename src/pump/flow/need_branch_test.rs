//! Need-branch names, base-once-per-run and tip, over the fake host.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{
    DELETING, FRESH, LIVE, PRUNED, RENAME_FAILED, RENAMING, branch, prior, record_base, register,
    tip,
};
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::RunRow;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

const MAIN_A: &str = "1111111111111111111111111111111111111111";
const MAIN_B: &str = "2222222222222222222222222222222222222222";
const NEED_TIP: &str = "3333333333333333333333333333333333333333";
const NEED: &str = "abcd1234";

struct Fixture {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture {
        _dir: dir,
        ctx,
        host,
        store,
    }
}

fn main_tip(host: &FakeHost, sha: &str) {
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{sha}\trefs/heads/main\n"),
    );
}

fn row(status: &str, run: u32) -> NeedBranchRow {
    NeedBranchRow {
        need: NEED.into(),
        branch: branch(NEED),
        base: None,
        status: status.into(),
        run,
        target: "default".into(),
        priors: vec![prior(NEED, 1)],
    }
}

#[test]
fn names_are_one_segment_after_their_prefix() {
    assert_eq!(branch(NEED), "vivi/need/abcd1234");
    let old = prior(NEED, 3);
    assert_eq!(old, "vivi/need-prior/abcd1234-r3");
    assert!(!old.strip_prefix("vivi/need-prior/").unwrap().contains('/'));
    assert_eq!(
        [FRESH, LIVE, RENAMING, DELETING, PRUNED],
        ["fresh", "live", "renaming", "deleting", "pruned"]
    );
}

#[test]
fn the_base_is_recorded_once_per_run() {
    let f = fixture();
    main_tip(&f.host, MAIN_A);
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap(),
        MAIN_A
    );
    main_tip(&f.host, MAIN_B);
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap(),
        MAIN_A
    );
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!(stored.base.as_deref(), Some(MAIN_A));
    assert_eq!(stored.status, LIVE);
    assert_eq!(stored.branch, "vivi/need/abcd1234");
    assert_eq!(stored.target, "default");
}

#[test]
fn a_fresh_row_records_a_new_base_and_keeps_its_priors() {
    let f = fixture();
    f.store.upsert_need_branch(&row(FRESH, 2)).unwrap();
    main_tip(&f.host, MAIN_B);
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 2, "default").unwrap(),
        MAIN_B
    );
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!(stored.status, LIVE);
    assert_eq!(stored.run, 2);
    assert_eq!(stored.priors, vec![prior(NEED, 1)]);
}

#[test]
fn record_base_errors_while_the_old_branch_is_moving() {
    for status in [RENAMING, DELETING] {
        let f = fixture();
        f.store.upsert_need_branch(&row(status, 1)).unwrap();
        main_tip(&f.host, MAIN_A);
        let err = record_base(&f.ctx, &f.store, NEED, 2, "default").unwrap_err();
        assert!(err.to_string().contains("still being moved"), "{err}");
        let stored = f.store.need_branch(NEED).unwrap().unwrap();
        assert_eq!(stored.status, status);
        assert_eq!(stored.base, None);
    }
}

#[test]
fn tip_is_the_base_until_the_branch_is_pushed() {
    let f = fixture();
    assert_eq!(tip(&f.ctx, &f.store, NEED).unwrap(), None);
    main_tip(&f.host, MAIN_A);
    record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap();
    f.host
        .reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    assert_eq!(
        tip(&f.ctx, &f.store, NEED).unwrap().as_deref(),
        Some(MAIN_A)
    );
    f.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{NEED_TIP}\trefs/heads/vivi/need/{NEED}\n"),
    );
    assert_eq!(
        tip(&f.ctx, &f.store, NEED).unwrap().as_deref(),
        Some(NEED_TIP)
    );
}

#[test]
fn tip_is_none_unless_the_row_is_live() {
    let f = fixture();
    for status in [FRESH, RENAMING, DELETING, PRUNED] {
        f.store.upsert_need_branch(&row(status, 1)).unwrap();
        assert_eq!(tip(&f.ctx, &f.store, NEED).unwrap(), None, "{status}");
    }
}

#[test]
fn an_ls_remote_failure_propagates() {
    let f = fixture();
    f.host.reply("ls-remote", 128, "fatal: unable to access");
    let err = record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap_err();
    assert!(err.to_string().contains("ls-remote exit 128"), "{err}");
    assert!(f.store.need_branch(NEED).unwrap().is_none());
}

// --- the restart phase ---------------------------------------------------

const OLD_TIP: &str = "4444444444444444444444444444444444444444";

#[derive(Default)]
struct Jobs(BTreeMap<String, PoolJob>);

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.0
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.0
    }
}

fn with_merge_role(f: Fixture) -> Fixture {
    let mut routing = (*f.ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    Fixture {
        ctx: f.ctx.with_routing(routing),
        ..f
    }
}

fn start_run(f: &Fixture, run: u32, entry: &str) {
    f.store
        .insert_run(&RunRow {
            need: NEED.into(),
            run,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: entry.into(),
            state: "live".into(),
        })
        .unwrap();
}

fn live_row(run: u32) -> NeedBranchRow {
    NeedBranchRow {
        base: Some(MAIN_A.into()),
        ..row(LIVE, run)
    }
}

fn tick(f: &Fixture, jobs: &mut Jobs) {
    let mut registry = Registry::<Jobs, FakeMail>::new();
    register(&mut registry);
    let mut mail = FakeMail::new("mind");
    for phase in registry.phases() {
        phase.run(&f.ctx, jobs, &f.store, &mut mail).unwrap();
    }
}

fn old_branch_at(f: &Fixture, sha: &str) {
    f.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{sha}\trefs/heads/vivi/need/{NEED}\n"),
    );
}

fn finish(jobs: &mut Jobs, id: &str, status: JobStatus, receipt: &str) {
    let job = jobs.0.get_mut(id).unwrap();
    job.status = status;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: receipt.into(),
        at: 1,
    });
}

fn alarms(f: &Fixture) -> usize {
    std::fs::read_to_string(f.ctx.home.join("exceptions.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(RENAME_FAILED))
        .count()
}

const PUBLISHED: &str = "runner publish: pushed refs/heads/vivi/need-prior/abcd1234-r1 \
4444444444444444444444444444444444444444 4444444444444444444444444444444444444444";
const DELETED: &str = "runner delete: deleted refs/heads/vivi/need/abcd1234 \
4444444444444444444444444444444444444444 4444444444444444444444444444444444444444 absent";

#[test]
fn a_rework_restart_keeps_the_branch_and_moves_the_run() {
    let f = with_merge_role(fixture());
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    start_run(&f, 2, "rework1");
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!((stored.run, stored.status.as_str()), (2, LIVE));
    assert_eq!(stored.base.as_deref(), Some(MAIN_A));
    assert!(jobs.0.is_empty());
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 2, "default").unwrap(),
        MAIN_A
    );
}

#[test]
fn a_plan_restart_renames_the_branch_then_deletes_the_old_name() {
    let f = with_merge_role(fixture());
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    start_run(&f, 2, "plan");
    old_branch_at(&f, OLD_TIP);
    let mut jobs = Jobs::default();

    tick(&f, &mut jobs);
    let rename = &jobs.0["need-prior-abcd1234-r1"];
    assert_eq!(rename.role, "merge");
    assert_eq!(rename.command.as_deref(), Some("true"));
    assert_eq!(rename.target.as_deref(), Some("default"));
    assert_eq!(rename.from_ref.as_deref(), Some(OLD_TIP));
    assert_eq!(rename.branch.as_deref(), Some(prior(NEED, 1).as_str()));
    assert_eq!(
        rename.publish_ref.as_deref(),
        Some("refs/heads/vivi/need-prior/abcd1234-r1")
    );
    assert_eq!(rename.publish_expect.as_deref(), Some(OLD_TIP));
    assert_eq!(f.store.need_branch(NEED).unwrap().unwrap().status, RENAMING);

    tick(&f, &mut jobs);
    assert_eq!(jobs.0.len(), 1, "a pending rename waits");

    finish(
        &mut jobs,
        "need-prior-abcd1234-r1",
        JobStatus::Finished,
        PUBLISHED,
    );
    tick(&f, &mut jobs);
    let cut = &jobs.0["need-cut-abcd1234-r1"];
    assert_eq!(cut.command.as_deref(), Some("true"));
    assert_eq!(cut.from_ref.as_deref(), Some(OLD_TIP));
    assert_eq!(
        cut.delete_ref.as_deref(),
        Some("refs/heads/vivi/need/abcd1234")
    );
    assert_eq!(cut.delete_expect.as_deref(), Some(OLD_TIP));
    assert_eq!(f.store.need_branch(NEED).unwrap().unwrap().status, DELETING);

    finish(
        &mut jobs,
        "need-cut-abcd1234-r1",
        JobStatus::Finished,
        DELETED,
    );
    tick(&f, &mut jobs);
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!((stored.run, stored.status.as_str()), (2, FRESH));
    assert_eq!(stored.base, None);
    assert_eq!(stored.priors, vec![prior(NEED, 1)]);
    assert_eq!(jobs.0.len(), 2);
    assert_eq!(alarms(&f), 0);
    tick(&f, &mut jobs);
    assert_eq!(jobs.0.len(), 2);
}

#[test]
fn a_restart_with_no_remote_branch_resets_the_row_at_once() {
    let f = with_merge_role(fixture());
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    start_run(&f, 2, "research");
    f.host
        .reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!((stored.run, stored.status.as_str()), (2, FRESH));
    assert_eq!(stored.base, None);
    assert_eq!(stored.priors, vec![prior(NEED, 1)]);
    assert!(jobs.0.is_empty());
}

#[test]
fn a_failed_rename_raises_once_and_leaves_the_row() {
    let f = with_merge_role(fixture());
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    start_run(&f, 2, "slice");
    old_branch_at(&f, OLD_TIP);
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    finish(
        &mut jobs,
        "need-prior-abcd1234-r1",
        JobStatus::Lost,
        "runner publish: refused refs/heads/x a b",
    );
    tick(&f, &mut jobs);
    tick(&f, &mut jobs);
    assert_eq!(alarms(&f), 1);
    assert_eq!(jobs.0.len(), 1);
    assert_eq!(f.store.need_branch(NEED).unwrap().unwrap().status, RENAMING);
}

#[test]
fn a_restart_without_a_merge_role_raises_and_changes_nothing() {
    let f = fixture();
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    start_run(&f, 2, "plan");
    old_branch_at(&f, OLD_TIP);
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    tick(&f, &mut jobs);
    assert_eq!(alarms(&f), 1);
    assert!(jobs.0.is_empty());
    assert_eq!(f.store.need_branch(NEED).unwrap().unwrap().status, LIVE);
}

#[test]
fn a_merge_role_without_a_pool_raises_and_enqueues_nothing() {
    let f = fixture();
    let mut routing = (*f.ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: None,
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    let f = Fixture {
        ctx: f.ctx.with_routing(routing),
        ..f
    };
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    start_run(&f, 2, "plan");
    old_branch_at(&f, OLD_TIP);
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    tick(&f, &mut jobs);
    assert_eq!(alarms(&f), 1);
    assert!(jobs.0.is_empty());
    assert_eq!(f.store.need_branch(NEED).unwrap().unwrap().status, LIVE);
}
