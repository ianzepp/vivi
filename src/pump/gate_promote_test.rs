//! Tests for the promotion rule ("main never gets redder": a gate promotes
//! when its failing set is within the set main carries), the persisted set,
//! and the gate forced by a merged fix.

use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::config::{GateConfig, PumpConfig};
use crate::pump::gate::{collect_completed, finish_gate, start_gate};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, PoolJob};
use crate::pump::state::{GateRecord, GateRun, Promotion, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::{RoleRouting, RoutedRole};

/// The fake clock's start.
const T0: u64 = 1_000_000;
/// A 40-hex remote tip.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
/// An earlier promoted tip.
const OLD: &str = "fedcba9876543210fedcba9876543210fedcba98";
/// A commit gated green before the red tip.
const GREEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The gate task handle the fake `task send` reports.
const TASK: &str = "1a2b3c4d";

fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        gate: GateConfig {
            command: String::from("stage 4b"),
            every_merges: 8,
            every_seconds: 1800,
            pools: vec![String::from("flygate")],
            model: String::from("m"),
            budget: 1800,
            ..GateConfig::default()
        },
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let ctx = ctx.with_routing(RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("release"),
            kind: Some(String::from("merge")),
            status: String::from("active"),
            pool: Some(String::from("flygate")),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: std::collections::BTreeMap::new(),
    });
    (dir, ctx, host)
}

/// A state with an open gate task at `TIP` and the given baseline.
fn state(baseline: &[&str]) -> State {
    State {
        merges: 4,
        baseline: Some(names(baseline)),
        last_gate: GateRun {
            at: T0,
            merges: 4,
            task: Some(String::from(TASK)),
            job_id: None,
            source_jobs: Vec::new(),
            tip: Some(String::from(TIP)),
            pool: Some(String::from("flygate")),
        },
        ..State::default()
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| String::from(*s)).collect()
}

fn reply(list: &[&str]) -> String {
    format!("FAILING-BEGIN\n{}\nFAILING-END\n", list.join("\n"))
}

fn promotion_job(st: &State) -> Option<&PoolJob> {
    st.jobs.values().find(|job| job.kind == "promote")
}

/// Feed a successful fast-forward receipt to the promotion collector.
fn confirm_promotion(ctx: &Ctx, st: &mut State, id: &str, tip: &str) {
    let job = st.jobs.get_mut(id).unwrap();
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: format!("pushed main {}", &tip[..10]),
        receipt: format!("PUMP_PROMOTE_RESULT={{\"status\":\"pushed\",\"tip\":\"{tip}\"}}"),
        at: ctx.now(),
    });
    collect_completed(ctx, st);
}

/// Feed a worker's recovery marker and verify its main-branch ancestry.
fn confirm_already_on_main(
    ctx: &Ctx,
    host: &Arc<FakeHost>,
    st: &mut State,
    id: &str,
    tip: &str,
    remote_tip: &str,
    ancestry_code: i32,
) {
    host.reply(
        "ls-remote origin refs/heads/main",
        0,
        &format!("{remote_tip}\trefs/heads/main\n"),
    );
    host.reply("cat-file -e", 0, "");
    host.reply("merge-base --is-ancestor", ancestry_code, "");
    let job = st.jobs.get_mut(id).unwrap();
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("no_changes"),
        receipt: format!(
            "PUMP_PROMOTE_RESULT={{\"status\":\"already-on-main\",\"tip\":\"{tip}\"}}"
        ),
        at: ctx.now(),
    });
    collect_completed(ctx, st);
}

fn promote_held(host: &Arc<FakeHost>) -> Option<String> {
    host.logs().into_iter().find(|l| l.contains("promote held"))
}

fn own(st: &mut State, name: &str) {
    st.red
        .insert(String::from(name), String::from("pumpfix-gx"));
}

#[test]
fn promotes_when_failing_is_within_main_even_with_owned_reds_outstanding() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: T0 - 100,
        failing: Some(names(&["a", "c", "d"])),
    });
    own(&mut st, "c");
    own(&mut st, "d");
    finish_gate(&ctx, &mut st, &reply(&["a", "c"]));
    let job = promotion_job(&st).expect("green gate queues promotion");
    assert_eq!(job.role, "release");
    let id = job.id.clone();
    confirm_promotion(&ctx, &mut st, &id, TIP);
    assert!(promote_held(&host).is_none());
    let last = st.promotions.last().unwrap();
    assert_eq!(last.tip, TIP);
    assert_eq!(last.failing, Some(names(&["a", "c"])));
    // d passed again and is released; c is still owned.
    assert!(!st.red.contains_key("d"));
    assert!(st.red.contains_key("c"));
    drop(dir);
}

#[test]
fn is_held_when_a_name_is_not_on_main_and_lists_up_to_five() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    let extra = ["x1", "x2", "x3", "x4", "x5", "x6", "x7"];
    for name in extra {
        own(&mut st, name);
    }
    let mut failing = vec!["a"];
    failing.extend(extra);
    finish_gate(&ctx, &mut st, &reply(&failing));
    assert!(promotion_job(&st).is_none());
    assert!(st.promotions.is_empty());
    assert_eq!(
        promote_held(&host).unwrap(),
        "promote held: 7 failures not on main at 0123456789 (main never gets redder): \
         x1; x2; x3; x4; x5 and 2 more"
    );
    drop(dir);
}

#[test]
fn main_set_follows_the_last_promotion() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a", "b"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    let job = promotion_job(&st).unwrap();
    let id = job.id.clone();
    confirm_promotion(&ctx, &mut st, &id, TIP);
    assert_eq!(st.promotions.len(), 1);
    assert_eq!(st.promotions[0].failing, Some(names(&["a"])));
    // b was on main by baseline, but main now carries only a.
    own(&mut st, "b");
    st.last_gate.task = Some(String::from(TASK));
    finish_gate(&ctx, &mut st, &reply(&["a", "b"]));
    assert_eq!(st.promotions.len(), 1);
    assert!(promote_held(&host).unwrap().contains(": b"));
    drop(dir);
}

#[test]
fn the_baseline_is_main_before_the_first_promotion() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a", "b"]);
    finish_gate(&ctx, &mut st, &reply(&["b"]));
    let job = promotion_job(&st).expect("the first green gate should queue promotion");
    let id = job.id.clone();
    confirm_promotion(&ctx, &mut st, &id, TIP);
    assert_eq!(st.promotions[0].failing, Some(names(&["b"])));
    drop(dir);
}

#[test]
fn a_promotion_without_a_recorded_set_uses_its_gate_record() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: T0 - 100,
        failing: None,
    });
    st.gates.push(GateRecord {
        tip: String::from(OLD),
        at: T0 - 110,
        merges: 2,
        names: Some(names(&["a", "g"])),
        new: None,
    });
    own(&mut st, "g");
    finish_gate(&ctx, &mut st, &reply(&["g"]));
    assert_eq!(promotion_job(&st).unwrap().from_ref.as_deref(), Some(TIP));
    drop(dir);
}

#[test]
fn the_recorded_set_round_trips_through_the_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let mut st = State::default();
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: 5,
        failing: Some(names(&["a", "b"])),
    });
    st.promotions.push(Promotion {
        tip: String::from(TIP),
        at: 9,
        failing: None,
    });
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&st).unwrap();
    let back = PumpStore::open(&path).unwrap().load().unwrap();
    assert_eq!(back.promotions[0].failing, Some(names(&["a", "b"])));
    assert_eq!(back.promotions[1].failing, None);
}

#[test]
fn a_database_made_before_the_set_was_recorded_gains_the_column() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE promotions;
         CREATE TABLE promotions (tip TEXT PRIMARY KEY, at INTEGER NOT NULL);
         INSERT INTO promotions (tip, at) VALUES ('t1', 3);",
    )
    .unwrap();
    drop(conn);
    let mut store = PumpStore::open(&path).unwrap();
    let mut st = store.load().unwrap();
    assert_eq!(st.promotions.len(), 1);
    assert_eq!(st.promotions[0].failing, None);
    st.promotions[0].failing = Some(names(&["z"]));
    store.save(&st).unwrap();
    let back = PumpStore::open(&path).unwrap().load().unwrap();
    assert_eq!(back.promotions[0].failing, Some(names(&["z"])));
}

fn wire_gate(host: &Arc<FakeHost>) {
    host.reply("pool status", 0, "slot flygate bridge slot-9 free\n");
    host.reply("git ls-remote", 0, &format!("{TIP}\trefs/heads/next\n"));
    host.reply("task send", 0, &format!("created hand {TASK}\nnode {TASK}"));
    host.reply(
        "pool dispatch",
        0,
        &format!("dispatched {TASK} bridge slot-9"),
    );
}

fn merged_fix(st: &mut State, uid: &str, at: u64) {
    st.units.insert(
        String::from(uid),
        Unit {
            status: Status::Merged,
            merged_at: Some(at),
            ..Unit::default()
        },
    );
}

fn quiet_state() -> State {
    let mut st = state(&["a"]);
    st.last_gate.task = None;
    st
}

fn forced_logs(host: &Arc<FakeHost>) -> usize {
    host.logs()
        .iter()
        .filter(|l| l.contains("gate forced: fix pumpfix-ga-1 merged"))
        .count()
}

#[test]
fn a_merged_fix_forces_one_gate_even_when_the_gate_is_not_due() {
    let (dir, ctx, host) = setup();
    let mut st = quiet_state();
    merged_fix(&mut st, "pumpfix-ga-1", T0 + 10);
    host.advance(20);
    // No pool is free yet: the flag is written once and kept.
    start_gate(&ctx, &mut st);
    start_gate(&ctx, &mut st);
    assert!(ctx.flag("FORCE-GATE"));
    assert_eq!(forced_logs(&host), 1);
    wire_gate(&host);
    start_gate(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    assert_eq!(st.last_gate.task.as_deref(), Some(TASK));
    // The gate started after the merge: the same fix forces nothing more.
    st.last_gate.task = None;
    start_gate(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    assert_eq!(forced_logs(&host), 1);
    drop(dir);
}

#[test]
fn a_fix_merged_before_the_last_gate_or_a_non_fix_unit_forces_nothing() {
    let (dir, ctx, host) = setup();
    let mut st = quiet_state();
    merged_fix(&mut st, "pumpfix-ga-1", T0 - 5);
    merged_fix(&mut st, "plain-1", T0 + 10);
    host.advance(20);
    start_gate(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    assert!(host.logs().iter().all(|l| !l.contains("gate forced")));
    drop(dir);
}

#[test]
fn promotion_is_a_durable_job_and_runs_no_local_git_push() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    let job = promotion_job(&st).expect("the gate queues a promotion role job");
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert_eq!(job.branch.as_deref(), Some("main"));
    assert!(host.commands().iter().all(|cmd| {
        !cmd.contains("git fetch") && !cmd.contains("merge-base") && !cmd.contains("git push")
    }));
    assert!(st.promotions.is_empty(), "the receipt has not landed");
    drop(dir);
}

#[test]
fn already_on_main_is_accepted_only_after_remote_ancestry_verification() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    let id = promotion_job(&st).unwrap().id.clone();
    let descendant = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    confirm_already_on_main(&ctx, &host, &mut st, &id, TIP, descendant, 0);
    assert_eq!(st.promotions.last().unwrap().tip, TIP);
    let commands = host.commands();
    assert!(
        commands
            .iter()
            .any(|cmd| { cmd.contains(&format!("merge-base --is-ancestor {TIP} {descendant}")) })
    );
    drop(dir);
}

#[test]
fn already_on_main_with_a_non_ancestor_tip_is_rejected() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    let id = promotion_job(&st).unwrap().id.clone();
    let other = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    confirm_already_on_main(&ctx, &host, &mut st, &id, TIP, other, 1);
    assert!(st.promotions.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("promote-job-failed"))
            || std::fs::read_to_string(ctx.home.join("exceptions.txt"))
                .unwrap()
                .contains("promote-job-failed")
    );
    drop(dir);
}

#[test]
fn unavailable_remote_ancestry_keeps_the_receipt_pending_for_retry() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    let id = promotion_job(&st).unwrap().id.clone();
    confirm_already_on_main(
        &ctx,
        &host,
        &mut st,
        &id,
        TIP,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        128,
    );
    assert!(st.promotions.is_empty());
    assert!(st.jobs.get(&id).unwrap().applied_at.is_none());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("promote verification unavailable"))
    );
    drop(dir);
}

#[test]
fn worker_push_marker_alone_does_not_count_without_remote_or_runner_proof() {
    let (dir, ctx, _host) = setup();
    let mut st = state(&["a"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    let id = promotion_job(&st).unwrap().id.clone();
    let job = st.jobs.get_mut(&id).unwrap();
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("no_changes"),
        receipt: format!("PUMP_PROMOTE_RESULT={{\"status\":\"pushed\",\"tip\":\"{TIP}\"}}"),
        at: ctx.now(),
    });
    collect_completed(&ctx, &mut st);
    assert!(st.promotions.is_empty());
    assert!(st.jobs.get(&id).unwrap().applied_at.is_none());
    drop(dir);
}

#[test]
fn main_ancestry_decisions_are_left_to_the_dispatched_promotion_role() {
    let (dir, ctx, host) = setup();
    host.reply("git merge-base", 128, "fatal: Not a valid commit name 0123");
    let mut st = state(&["a"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    assert!(promotion_job(&st).is_some());
    assert!(host.commands().iter().all(|cmd| !cmd.contains("git push")));
    assert!(
        host.logs()
            .iter()
            .all(|line| !line.contains("promote error"))
    );
    drop(dir);
}

fn beta(ctx: &Ctx) -> Ctx {
    let mut target = ctx.clone();
    target.target_id = String::from("beta");
    target
}

#[test]
fn a_merged_fix_forces_only_its_own_targets_flag() {
    let (dir, ctx, host) = setup();
    let beta = beta(&ctx);
    let mut st = quiet_state();
    merged_fix(&mut st, "pumpfix-ga-1", T0 + 10);
    st.units.get_mut("pumpfix-ga-1").unwrap().target = String::from("beta");
    host.advance(20);
    start_gate(&beta, &mut st);
    assert!(beta.flag("FORCE-GATE-beta"));
    assert!(!ctx.flag("FORCE-GATE"));
    // The default target sees no merged fix of its own.
    start_gate(&ctx, &mut st);
    assert!(!ctx.flag("FORCE-GATE"));
    drop(dir);
}

#[test]
fn a_forced_gate_is_consumed_only_by_its_own_target() {
    let (dir, ctx, host) = setup();
    wire_gate(&host);
    let beta = beta(&ctx);
    // The operator's manual FORCE-GATE belongs to the default target.
    std::fs::write(ctx.home.join("FORCE-GATE"), "").unwrap();
    let mut other = quiet_state();
    start_gate(&beta, &mut other);
    assert_eq!(other.last_gate.task, None);
    assert!(ctx.flag("FORCE-GATE"));
    let mut st = quiet_state();
    start_gate(&ctx, &mut st);
    assert_eq!(st.last_gate.task.as_deref(), Some(TASK));
    assert!(!ctx.flag("FORCE-GATE"));
    // The beta target is forced by its own suffixed flag.
    std::fs::write(ctx.home.join("FORCE-GATE-beta"), "").unwrap();
    start_gate(&ctx, &mut quiet_state());
    assert!(ctx.flag("FORCE-GATE-beta"));
    start_gate(&beta, &mut other);
    assert_eq!(other.last_gate.task.as_deref(), Some(TASK));
    assert!(!ctx.flag("FORCE-GATE-beta"));
    drop(dir);
}

fn unknown_set_logs(host: &Arc<FakeHost>) -> Vec<String> {
    host.logs()
        .into_iter()
        .filter(|l| l.contains("main's failing set unknown"))
        .collect()
}

#[test]
fn an_unknown_main_set_is_logged_once_per_tip_and_falls_back_to_the_baseline() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: T0 - 100,
        failing: None,
    });
    own(&mut st, "x");
    finish_gate(&ctx, &mut st, &reply(&["a", "x"]));
    // Held against the baseline: x is not on main by the baseline set.
    assert!(promotion_job(&st).is_none());
    let logs = unknown_set_logs(&host);
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains("fedcba9876") && logs[0].contains("using baseline"));
    st.last_gate.task = Some(String::from(TASK));
    finish_gate(&ctx, &mut st, &reply(&["a", "x"]));
    assert_eq!(unknown_set_logs(&host).len(), 1);
    drop(dir);
}

#[test]
fn a_known_or_absent_promotion_never_logs_an_unknown_set() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    st.last_gate.task = Some(String::from(TASK));
    finish_gate(&ctx, &mut st, &reply(&["a"]));
    assert!(unknown_set_logs(&host).is_empty());
    drop(dir);
}

#[test]
fn a_red_tip_promotes_the_newest_green_commit_below_it() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: T0 - 100,
        failing: Some(names(&["a"])),
    });
    st.gates.push(GateRecord {
        tip: String::from(GREEN),
        at: T0 - 200,
        merges: 2,
        names: Some(names(&["a"])),
        new: Some(vec![]),
    });
    own(&mut st, "x");
    finish_gate(&ctx, &mut st, &reply(&["a", "x"]));
    let job = promotion_job(&st).expect("last green tip should be queued for promotion");
    assert_eq!(job.from_ref.as_deref(), Some(GREEN));
    let id = job.id.clone();
    confirm_promotion(&ctx, &mut st, &id, GREEN);
    let last = st.promotions.last().unwrap();
    assert_eq!(last.tip, GREEN);
    assert_eq!(last.failing, Some(names(&["a"])));
    assert!(promote_held(&host).is_some());
    drop(dir);
}

#[test]
fn a_red_tip_with_new_failures_still_promotes_the_last_green_gate() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    st.promotions.push(Promotion {
        tip: String::from(OLD),
        at: T0 - 100,
        failing: Some(names(&["a"])),
    });
    st.gates.push(GateRecord {
        tip: String::from(GREEN),
        at: T0 - 200,
        merges: 2,
        names: Some(names(&["a"])),
        new: Some(vec![]),
    });
    finish_gate(&ctx, &mut st, &reply(&["a", "x"]));
    let job = promotion_job(&st).expect("last green tip should be queued for promotion");
    assert_eq!(job.from_ref.as_deref(), Some(GREEN));
    let id = job.id.clone();
    confirm_promotion(&ctx, &mut st, &id, GREEN);
    assert_eq!(st.promotions.last().unwrap().tip, GREEN);
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("gate-new-failures"), "{exceptions}");
    drop(dir);
}

#[test]
fn a_green_candidate_main_already_sits_at_is_not_pushed_again() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    st.promotions.push(Promotion {
        tip: String::from(GREEN),
        at: T0 - 100,
        failing: Some(names(&["a"])),
    });
    st.gates.push(GateRecord {
        tip: String::from(GREEN),
        at: T0 - 200,
        merges: 2,
        names: Some(names(&["a"])),
        new: Some(vec![]),
    });
    own(&mut st, "x");
    finish_gate(&ctx, &mut st, &reply(&["a", "x"]));
    assert!(promotion_job(&st).is_none());
    assert_eq!(st.promotions.len(), 1);
    drop(dir);
}

#[test]
fn a_red_tip_with_no_green_gate_below_promotes_nothing() {
    let (dir, ctx, host) = setup();
    let mut st = state(&["a"]);
    own(&mut st, "x");
    finish_gate(&ctx, &mut st, &reply(&["a", "x"]));
    assert!(promotion_job(&st).is_none());
    assert!(st.promotions.is_empty());
    assert!(promote_held(&host).is_some());
    drop(dir);
}
