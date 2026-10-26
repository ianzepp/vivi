//! Need-branch names, base-once-per-run and tip, over the fake host.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{
    DELETING, FRESH, LIVE, PRUNED, RENAME_FAILED, RENAMING, branch, observed_tip, prior,
    record_base, register, tip,
};
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::fanout::{VERDICT_MERGED, VERDICT_NO_DEFECT};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
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

fn absent_branch(f: &Fixture) {
    f.host.reply("ls-remote origin refs/heads/vivi/need/", 0, "");
}

fn branch_at(f: &Fixture, sha: &str) {
    f.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{sha}\trefs/heads/vivi/need/{NEED}\n"),
    );
}

fn seed_run(f: &Fixture, run: u32) {
    f.store
        .insert_run(&RunRow {
            need: NEED.into(),
            run,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        })
        .unwrap();
}

fn seed_slice(f: &Fixture, run: u32, node: &str, task: &str, verdict: &str) {
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run,
                node: node.into(),
                task: task.into(),
                state: StepState::Done,
                verdict: Some(verdict.into()),
            },
        )
        .unwrap();
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
    start_run(&f, 1, "plan");
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
    start_run(&f, 2, "plan");
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
    start_run(&f, 1, "plan");
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
    for status in [FRESH, PRUNED] {
        let f = fixture();
        f.store.upsert_need_branch(&row(status, 1)).unwrap();
        assert_eq!(tip(&f.ctx, &f.store, NEED).unwrap(), None, "{status}");
    }
    for status in [RENAMING, DELETING] {
        let f = fixture();
        f.store.upsert_need_branch(&row(status, 1)).unwrap();
        let err = tip(&f.ctx, &f.store, NEED).unwrap_err();
        assert!(err.to_string().contains("still being moved"), "{status}: {err}");
    }
}

#[test]
fn a_published_tip_is_returned_over_recorded_merged_history() {
    let f = fixture();
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    start_run(&f, 1, "plan");
    seed_slice(&f, 1, "slice:s1", "t-s1", VERDICT_MERGED);
    branch_at(&f, NEED_TIP);
    assert_eq!(
        tip(&f.ctx, &f.store, NEED).unwrap().as_deref(),
        Some(NEED_TIP)
    );
}

#[test]
fn same_run_merged_history_without_a_publication_is_refused() {
    let f = fixture();
    start_run(&f, 1, "plan");
    main_tip(&f.host, MAIN_A);
    record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap();
    seed_slice(&f, 1, "slice:s1", "t-s1", VERDICT_MERGED);
    absent_branch(&f);
    let err = tip(&f.ctx, &f.store, NEED).unwrap_err();
    assert!(err.to_string().contains("unresolved"), "{err}");
    // Publishing the branch resolves the same history.
    branch_at(&f, NEED_TIP);
    assert_eq!(
        tip(&f.ctx, &f.store, NEED).unwrap().as_deref(),
        Some(NEED_TIP)
    );
}

#[test]
fn archive_only_merged_history_without_a_publication_is_refused() {
    let f = fixture();
    f.store.upsert_need_branch(&live_row(2)).unwrap();
    start_run(&f, 1, "plan");
    start_run(&f, 2, "plan");
    seed_slice(&f, 1, "slice:s1", "t-s1", VERDICT_MERGED);
    absent_branch(&f);
    let err = tip(&f.ctx, &f.store, NEED).unwrap_err();
    assert!(err.to_string().contains("unresolved"), "{err}");
}

#[test]
fn no_defect_history_never_requires_publication() {
    let f = fixture();
    start_run(&f, 1, "plan");
    start_run(&f, 2, "plan");
    seed_slice(&f, 1, "slice:s1", "t-s1", VERDICT_NO_DEFECT);
    f.store.upsert_need_branch(&live_row(2)).unwrap();
    absent_branch(&f);
    assert_eq!(
        tip(&f.ctx, &f.store, NEED).unwrap().as_deref(),
        Some(MAIN_A)
    );
}

#[test]
fn a_pruned_row_resolves_to_none_even_with_merged_history() {
    let f = fixture();
    start_run(&f, 1, "plan");
    seed_slice(&f, 1, "slice:s1", "t-s1", VERDICT_MERGED);
    f.store.upsert_need_branch(&row(PRUNED, 1)).unwrap();
    absent_branch(&f);
    assert_eq!(tip(&f.ctx, &f.store, NEED).unwrap(), None);
}

#[test]
fn merged_history_without_a_row_is_refused() {
    let f = fixture();
    start_run(&f, 1, "plan");
    seed_slice(&f, 1, "slice:s1", "t-s1", VERDICT_MERGED);
    let err = tip(&f.ctx, &f.store, NEED).unwrap_err();
    assert!(err.to_string().contains("no need-branch row"), "{err}");
    // No-defect history is not published code: a missing row stays None.
    let g = fixture();
    start_run(&g, 1, "plan");
    seed_slice(&g, 1, "slice:s1", "t-s1", VERDICT_NO_DEFECT);
    assert_eq!(tip(&g.ctx, &g.store, NEED).unwrap(), None);
}

#[test]
fn observed_tip_accepts_the_callers_observation() {
    let f = fixture();
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    start_run(&f, 1, "plan");
    seed_slice(&f, 1, "slice:s1", "t-s1", VERDICT_MERGED);
    // The caller's observation is returned as the published tip.
    assert_eq!(
        observed_tip(&f.ctx, &f.store, NEED, Some(NEED_TIP.into()))
            .unwrap()
            .as_deref(),
        Some(NEED_TIP)
    );
    // An absent observation over the same merged history is refused.
    let err = observed_tip(&f.ctx, &f.store, NEED, None).unwrap_err();
    assert!(err.to_string().contains("unresolved"), "{err}");
    // An unknown target is refused even before the observation matters.
    let other = NeedBranchRow {
        target: "other".into(),
        ..live_row(1)
    };
    f.store.upsert_need_branch(&other).unwrap();
    let err = observed_tip(&f.ctx, &f.store, NEED, Some(NEED_TIP.into())).unwrap_err();
    assert!(err.to_string().contains("unknown target other"), "{err}");
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

#[test]
fn a_tip_ls_remote_failure_propagates() {
    let f = fixture();
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    f.host.reply("ls-remote", 128, "fatal: unable to access");
    let err = tip(&f.ctx, &f.store, NEED).unwrap_err();
    assert!(err.to_string().contains("ls-remote exit 128"), "{err}");
}

#[test]
fn a_tip_for_an_unknown_target_is_refused() {
    let f = fixture();
    f.store
        .upsert_need_branch(&NeedBranchRow {
            target: "other".into(),
            ..live_row(1)
        })
        .unwrap();
    let err = tip(&f.ctx, &f.store, NEED).unwrap_err();
    assert!(err.to_string().contains("unknown target other"), "{err}");
}

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
fn ordinary_restarts_adopt_the_live_row() {
    for entry in ["research", "plan", "slice", "rework1"] {
        // No merge role: adoption needs neither a role nor a pool.
        let f = fixture();
        f.store.upsert_need_branch(&live_row(1)).unwrap();
        start_run(&f, 2, entry);
        let mut jobs = Jobs::default();
        tick(&f, &mut jobs);
        let stored = f.store.need_branch(NEED).unwrap().unwrap();
        assert_eq!(
            (stored.need.as_str(), stored.run, stored.status.as_str()),
            (NEED, 2, LIVE),
            "{entry}"
        );
        assert_eq!(stored.base.as_deref(), Some(MAIN_A), "{entry}");
        assert_eq!(stored.branch, branch(NEED), "{entry}");
        assert_eq!(stored.target, "default", "{entry}");
        assert_eq!(stored.priors, vec![prior(NEED, 1)], "{entry}");
        assert!(jobs.0.is_empty(), "{entry}");
        assert_eq!(alarms(&f), 0, "{entry}");
        // Ticking again adopts nothing anew: the row is already this run's.
        tick(&f, &mut jobs);
        assert_eq!(
            f.store.need_branch(NEED).unwrap().unwrap(),
            stored,
            "{entry}"
        );
        // A further restart adopts the same row once more, still unchanged.
        start_run(&f, 3, entry);
        tick(&f, &mut jobs);
        let again = f.store.need_branch(NEED).unwrap().unwrap();
        assert_eq!((again.run, again.status.as_str()), (3, LIVE), "{entry}");
        assert_eq!(again.base.as_deref(), Some(MAIN_A), "{entry}");
        assert_eq!(again.branch, branch(NEED), "{entry}");
        assert_eq!(again.target, "default", "{entry}");
        assert_eq!(again.priors, vec![prior(NEED, 1)], "{entry}");
        assert!(jobs.0.is_empty(), "{entry}");
        assert_eq!(alarms(&f), 0, "{entry}");
    }
}

#[test]
fn the_base_survives_a_restart_and_main_advancing() {
    let f = fixture();
    start_run(&f, 1, "plan");
    main_tip(&f.host, MAIN_A);
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap(),
        MAIN_A
    );
    // Main moves on and a new run starts before the restart phase has
    // ticked: the base of run 1 is still the base of run 2.
    main_tip(&f.host, MAIN_B);
    start_run(&f, 2, "plan");
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 2, "default").unwrap(),
        MAIN_A
    );
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!((stored.run, stored.base.as_deref()), (1, Some(MAIN_A)));
    // Adoption moves the run forward without touching the base.
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    let adopted = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!((adopted.run, adopted.status.as_str()), (2, LIVE));
    assert_eq!(adopted.base.as_deref(), Some(MAIN_A));
    assert_eq!(
        record_base(&f.ctx, &f.store, NEED, 2, "default").unwrap(),
        MAIN_A
    );
}

#[test]
fn an_older_run_cannot_take_ownership_back() {
    let f = fixture();
    f.store.upsert_need_branch(&live_row(2)).unwrap();
    main_tip(&f.host, MAIN_B);
    let err = record_base(&f.ctx, &f.store, NEED, 1, "default").unwrap_err();
    assert!(err.to_string().contains("newer than run 1"), "{err}");
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!(
        (stored.run, stored.status.as_str(), stored.base.as_deref()),
        (2, LIVE, Some(MAIN_A))
    );
}

#[test]
fn a_base_for_another_target_is_refused_without_mutation() {
    let f = fixture();
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    main_tip(&f.host, MAIN_B);
    let err = record_base(&f.ctx, &f.store, NEED, 1, "other").unwrap_err();
    assert!(err.to_string().contains("lives on target default"), "{err}");
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!(
        (
            stored.run,
            stored.status.as_str(),
            stored.target.as_str(),
            stored.base.as_deref()
        ),
        (1, LIVE, "default", Some(MAIN_A))
    );
}

fn renaming_row(run: u32) -> NeedBranchRow {
    NeedBranchRow {
        status: RENAMING.into(),
        ..live_row(run)
    }
}

#[test]
fn a_seeded_renaming_row_finishes_its_move_then_resets() {
    let f = with_merge_role(fixture());
    f.store.upsert_need_branch(&renaming_row(1)).unwrap();
    start_run(&f, 2, "plan");
    branch_at(&f, OLD_TIP);
    let mut jobs = Jobs::default();

    // The move job is missing, so recovery enqueues it again.
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
fn a_seeded_deleting_row_with_no_remote_branch_resets_at_once() {
    let f = with_merge_role(fixture());
    f.store
        .upsert_need_branch(&NeedBranchRow {
            status: DELETING.into(),
            ..live_row(1)
        })
        .unwrap();
    start_run(&f, 2, "research");
    f.host
        .reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    let stored = f.store.need_branch(NEED).unwrap().unwrap();
    assert_eq!(
        (stored.need.as_str(), stored.run, stored.status.as_str()),
        (NEED, 2, FRESH)
    );
    assert_eq!(stored.base, None);
    assert_eq!(stored.priors, vec![prior(NEED, 1)]);
    assert!(jobs.0.is_empty());
}

#[test]
fn a_failed_rename_raises_once_and_leaves_the_row() {
    let f = with_merge_role(fixture());
    f.store.upsert_need_branch(&renaming_row(1)).unwrap();
    start_run(&f, 2, "slice");
    branch_at(&f, OLD_TIP);
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
    f.store.upsert_need_branch(&renaming_row(1)).unwrap();
    start_run(&f, 2, "plan");
    branch_at(&f, OLD_TIP);
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    tick(&f, &mut jobs);
    assert_eq!(alarms(&f), 1);
    assert!(jobs.0.is_empty());
    assert_eq!(f.store.need_branch(NEED).unwrap().unwrap().status, RENAMING);
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
    f.store.upsert_need_branch(&renaming_row(1)).unwrap();
    start_run(&f, 2, "plan");
    branch_at(&f, OLD_TIP);
    let mut jobs = Jobs::default();
    tick(&f, &mut jobs);
    tick(&f, &mut jobs);
    assert_eq!(alarms(&f), 1);
    assert!(jobs.0.is_empty());
    assert_eq!(f.store.need_branch(NEED).unwrap().unwrap().status, RENAMING);
}

#[test]
fn an_unreadable_row_does_not_stop_the_next_needs_restart() {
    let f = with_merge_role(fixture());
    const OTHER: &str = "ef567890";
    f.store.upsert_need_branch(&live_row(1)).unwrap();
    f.store
        .upsert_need_branch(&NeedBranchRow {
            need: OTHER.into(),
            branch: branch(OTHER),
            priors: Vec::new(),
            ..live_row(1)
        })
        .unwrap();
    // The first need's stored record no longer decodes.
    let conn = rusqlite::Connection::open(f._dir.path().join(DB_FILENAME)).unwrap();
    conn.execute(
        "UPDATE need_branches SET record_json = 'not json' WHERE need = ?1",
        [NEED],
    )
    .unwrap();
    for need in [NEED, OTHER] {
        f.store
            .insert_run(&RunRow {
                need: need.into(),
                run: 2,
                flow: "build".into(),
                flow_version: 2,
                kind: "build".into(),
                entry: "rework1".into(),
                state: "live".into(),
            })
            .unwrap();
    }
    let mut registry = Registry::<Jobs, FakeMail>::new();
    register(&mut registry);
    let mut jobs = Jobs::default();
    let mut mail = FakeMail::new("mind");
    let results: Vec<_> = registry
        .phases()
        .iter()
        .map(|phase| phase.run(&f.ctx, &mut jobs, &f.store, &mut mail))
        .collect();
    assert!(results.iter().any(Result::is_err), "{results:?}");
    assert_eq!(f.store.need_branch(OTHER).unwrap().unwrap().run, 2);
}
