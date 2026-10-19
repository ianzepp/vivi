//! Tests for [`crate::pump::collect`]: the reconcile parser and the
//! in-flight unit state machine, driven through [`FakeHost`] replies.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::collect::*;
use crate::pump::config::{PlannerConfig, PumpConfig, PumpTarget};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{Attempt, GateRun, State, Status, Unit};

/// The fake clock's start; units are created "now".
const T0: u64 = 1_000_000;

/// A context with default config, home at `dir/home`.
fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    setup_with(PumpConfig::default())
}

/// A context with `planner` swapped into an otherwise default config.
fn setup_with(cfg: PumpConfig) -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// A planner config that marks its charter in the spawned command line.
fn planner_cfg() -> PumpConfig {
    PumpConfig {
        planner: PlannerConfig {
            run: String::from("plan-it {charter} --quiet"),
            charter: String::from("/charter/path.md"),
            split_hard_cap: 1500,
        },
        ..PumpConfig::default()
    }
}

/// One unit at the fake clock's start.
fn unit(status: Status, task: Option<&str>, branch: Option<&str>) -> Unit {
    Unit {
        status,
        task: task.map(str::to_owned),
        branch: branch.map(str::to_owned),
        since: T0,
        ..Unit::default()
    }
}

/// Insert `unit` under `uid` into `st`.
fn put(st: &mut State, uid: &str, unit: Unit) {
    st.units.insert(uid.to_owned(), unit);
}

/// The recorded exception lines (`<home>/exceptions.txt`).
fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

/// The resplit record's `worker_said` value.
fn worker_said(ctx: &Ctx, uid: &str) -> String {
    let text = fs::read_to_string(ctx.home.join(format!("resplit/{uid}.json"))).unwrap();
    text.split("\"worker_said\": \"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn parse_reconcile_reads_reconciled_and_collected_lines() {
    let out = "collected 1a2b3c4d bridge slot-1 finished \"pushed unit/x-a1 abc\"\n\
               reconciled 9f8e7d6c bridge slot-2 running \"agent pid 42\" extra\n\
               noise line ignored\n\
               collected toofew tokens\n";
    let seen = parse_reconcile(out);
    assert_eq!(seen.len(), 2);
    let (state, detail) = seen.get("1a2b3c4d").unwrap();
    assert_eq!(state, "finished");
    assert_eq!(detail, "pushed unit/x-a1 abc");
    let (state, detail) = seen.get("9f8e7d6c").unwrap();
    assert_eq!(state, "running");
    assert_eq!(detail, "agent pid 42");
}

#[test]
fn startup_recovery_adopts_a_pushed_branch_for_the_next_attempt() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "ls-remote --heads",
        0,
        concat!(
            "abc123\trefs/heads/unit/orphan-a1\n",
            "def456\trefs/heads/unit/retry-a1\n",
            "ghi789\trefs/heads/unit/retry-next-a2\n",
        ),
    );
    let mut st = State::default();
    put(&mut st, "orphan", unit(Status::Pending, None, None));
    let mut retried = unit(Status::Pending, None, Some("unit/retry-a1"));
    retried.attempts = 1;
    put(&mut st, "retry", retried);
    let mut retried_next = unit(Status::Pending, None, None);
    retried_next.attempts = 1;
    put(&mut st, "retry-next", retried_next);

    recover_pushed_pending(&ctx, &mut st).unwrap();

    let orphan = &st.units["orphan"];
    assert_eq!(orphan.status, Status::Delivered);
    assert_eq!(orphan.attempts, 1);
    assert_eq!(orphan.branch.as_deref(), Some("unit/orphan-a1"));
    assert_eq!(st.units["retry"].status, Status::Pending);
    let retry_next = &st.units["retry-next"];
    assert_eq!(retry_next.status, Status::Delivered);
    assert_eq!(retry_next.attempts, 2);
    assert_eq!(retry_next.branch.as_deref(), Some("unit/retry-next-a2"));
    assert_eq!(
        host.commands()
            .iter()
            .filter(|command| command.contains("ls-remote --heads"))
            .count(),
        1
    );
}

#[test]
fn a_running_unit_stays_inflight() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 running \"agent pid 7\"",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    assert_eq!(collect(&ctx, &mut st), None);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Inflight);
    assert_eq!(
        st.units.get("u-1").unwrap().task.as_deref(),
        Some("1a2b3c4d")
    );
}

#[test]
fn a_running_unit_past_budget_plus_grace_is_split() {
    let (dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 running \"agent pid 7\"",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    host.advance(600 + 900 + 1);
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Splitting);
    assert_eq!(u.task, None);
    assert_eq!(u.history.last().unwrap().why, "timeout");
    assert_eq!(
        worker_said(&ctx, "u-1"),
        "overdue: still running long past its budget"
    );
    assert!(dir.path().join("home/resplit/u-1.prompt").exists());
}

#[test]
fn a_timeout_state_requests_a_split_and_writes_the_resplit_files() {
    let (dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 timeout \"budget exceeded\"",
    );
    host.reply("task show", 0, "the worker ran out of time");
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Splitting);
    assert_eq!(u.task, None);
    assert_eq!(u.since, T0);
    assert_eq!(u.history.last().unwrap().why, "timeout");
    assert_eq!(worker_said(&ctx, "u-1"), "the worker ran out of time");
    let prompt = fs::read_to_string(dir.path().join("home/resplit/u-1.prompt")).unwrap();
    assert!(prompt.contains("home/resplit/u-1.json"));
    assert!(prompt.contains("home/plans/u-1.split.json"));
    assert!(prompt.contains("\"replaces\": \"u-1\""));
    assert_eq!(host.spawned().len(), 1);
}

#[test]
fn a_finished_unit_whose_branch_is_on_the_remote_is_delivered() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"pushed unit/x-a1 abc\"",
    );
    host.reply("ls-remote", 0, "abc123\trefs/heads/unit/x-a1\n");
    host.reply("task show", 0, "all good, pushed");
    let mut st = State::default();
    let mut u = unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1"));
    u.model = Some(String::from("zai/glm-5.3-flash"));
    u.pool = Some(String::from("fly4"));
    u.since_dispatch = Some(T0 - 500);
    put(&mut st, "u-1", u);
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Delivered);
    assert_eq!(u.since, T0);
    assert_eq!(
        host.logs().last().unwrap(),
        "delivered u-1 unit/x-a1 secs=500 model=zai/glm-5.3-flash pool=fly4"
    );
}

#[test]
fn a_local_pool_branch_is_delivered_from_its_worktree_repository() {
    let (_dir, ctx, host) = setup();
    let ctx = ctx.with_pool_sources(BTreeMap::from([(
        String::from("glm"),
        PathBuf::from("/wtree/repo"),
    )]));
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d glm slot-1 finished \"committed unit/x-a1 abc\"",
    );
    host.reply("/wtree/repo refs/heads/unit/x-a1", 0, "abc123\trefs/heads/unit/x-a1\n");
    host.reply("task show", 0, "done, committed");
    let mut st = State::default();
    let mut u = unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1"));
    u.pool = Some(String::from("glm"));
    u.since_dispatch = Some(T0 - 500);
    put(&mut st, "u-1", u);
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Delivered);
    assert!(
        host.commands()
            .iter()
            .any(|command| command.contains("ls-remote /wtree/repo refs/heads/unit/x-a1"))
    );
}

#[test]
fn a_local_target_branch_is_collected_from_its_integration_clone() {
    let mut cfg = PumpConfig::default();
    cfg.targets.insert(
        String::from("tela"),
        PumpTarget {
            path: String::from("tela"),
            branch: String::from("main"),
            ..PumpTarget::default()
        },
    );
    let (_dir, ctx, host) = setup_with(cfg);
    host.reply(
        "pool reconcile",
        0,
        "collected task-1 bridge slot-1 finished \"committed unit/n-1-a1 abc\"\n",
    );
    host.reply("show-ref --verify --quiet refs/heads/unit/n-1-a1", 0, "");
    let mut unit = unit(Status::Inflight, Some("task-1"), Some("unit/n-1-a1"));
    unit.target = String::from("tela");
    let mut st = State::default();
    put(&mut st, "n-1", unit);
    collect(&ctx, &mut st);
    assert_eq!(st.units["n-1"].status, Status::Delivered);
    assert!(
        host.commands().iter().any(|command| {
            command.contains("show-ref --verify --quiet refs/heads/unit/n-1-a1")
        })
    );
}

#[test]
fn a_finished_unit_with_exit_124_in_the_receipt_is_split_even_with_a_branch() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"pushed unit/x-a1 abc\"",
    );
    host.reply("ls-remote", 0, "abc123\trefs/heads/unit/x-a1\n");
    host.reply("task show", 0, "agent exit 124 after 600s");
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Splitting);
    assert_eq!(u.task, None);
    assert_eq!(u.history.last().unwrap().why, "timeout");
}

#[test]
fn timeout_exit_codes_split_with_or_without_a_branch() {
    for code in [124, 137] {
        for branch_exists in [false, true] {
            for receipt in [
                format!("agent exit {code} after 600s"),
                format!("runner log\nagent_rc {code} agent_s 600"),
            ] {
                let (_dir, ctx, host) = setup();
                host.reply(
                    "pool reconcile",
                    0,
                    "collected 1a2b3c4d bridge slot-1 finished \"done\"",
                );
                host.reply(
                    "ls-remote",
                    0,
                    if branch_exists {
                        "abc refs/heads/unit/x-a1"
                    } else {
                        ""
                    },
                );
                host.reply("task show", 0, &receipt);
                let mut st = State::default();
                put(
                    &mut st,
                    "u-1",
                    unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
                );
                collect(&ctx, &mut st);
                let u = &st.units["u-1"];
                assert_eq!(
                    u.status,
                    Status::Splitting,
                    "{receipt}; branch={branch_exists}"
                );
                assert_eq!(u.task, None);
                assert_eq!(u.history.last().unwrap().why, "timeout");
            }
        }
    }
}

#[test]
fn a_successful_receipt_quoting_timeouts_is_delivered() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"pushed unit/x-a1 abc\"",
    );
    host.reply("ls-remote", 0, "abc123 refs/heads/unit/x-a1");
    host.reply(
        "task show",
        0,
        "agent exit 0 after 10s\n\n--- receipt\nFixed the previous agent exit 124 and agent exit 137.\nQuoted runner: agent_rc 124\n  agent_rc 137\nagent_rc 1240\nagent_rc 1370",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    collect(&ctx, &mut st);
    assert_eq!(st.units["u-1"].status, Status::Delivered);
    assert!(!ctx.home.join("resplit/u-1.json").exists());
}

#[test]
fn a_finished_unit_without_a_branch_and_with_the_no_defect_marker_becomes_no_defect_and_raises_an_exception()
 {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"no push\"",
    );
    host.reply("ls-remote", 0, "");
    host.reply(
        "task show",
        0,
        "VERDICT: no-defect\nthe code is already correct",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::NoDefect);
    assert_eq!(
        u.verdict_text.as_deref(),
        Some("VERDICT: no-defect\nthe code is already correct")
    );
    assert!(exceptions(&ctx).contains("no-defect u-1 VERDICT: no-defect"));
}

#[test]
fn a_finished_unit_without_a_branch_fails_and_goes_back_to_pending_with_a_history_record() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"pushed nothing\"",
    );
    host.reply("ls-remote", 0, "");
    host.reply("task show", 0, "agent replied: did nothing");
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Pending);
    assert_eq!(u.task, None);
    let record = u.history.last().unwrap();
    assert_eq!(record.attempt, 0);
    assert_eq!(record.why, "finished/pushed nothing");
    assert_eq!(record.detail, "agent replied: did nothing");
    assert_eq!(record.at, T0);
}

#[test]
fn the_third_failed_attempt_makes_the_unit_stuck_and_raises_an_exception() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"pushed nothing\"",
    );
    host.reply("ls-remote", 0, "");
    host.reply("task show", 0, "broke it again");
    let mut st = State::default();
    let mut u = unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1"));
    u.attempts = 3;
    put(&mut st, "u-1", u);
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Stuck);
    assert_eq!(u.task, None);
    assert_eq!(u.history.last().unwrap().why, "finished/pushed nothing");
    assert!(exceptions(&ctx).contains("stuck u-1 finished/pushed nothing: broke it again"));
}

#[test]
fn a_transient_failure_retries_without_sticking() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "reconciled t1 pool slot error host-down",
    );
    let mut st = State::default();
    let mut u = unit(Status::Inflight, Some("t1"), Some("unit/x-a5"));
    u.attempts = 5;
    put(&mut st, "u-1", u);
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Pending, "transient never sticks");
    assert_eq!(u.transient_retries, 1);
    assert_eq!(u.retry_not_before, Some(T0 + 120));
    assert_eq!(u.task, None);
    assert!(!exceptions(&ctx).contains("stuck"));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("transient-retry u-1 host 1/2")),
        "logs: {:?}",
        host.logs()
    );
}

#[test]
fn transient_retries_escalate_at_the_cap() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "reconciled t1 pool slot error host-down",
    );
    let mut st = State::default();
    let mut u = unit(Status::Inflight, Some("t1"), Some("unit/x-a5"));
    u.attempts = 5;
    u.transient_retries = ctx.cfg.dispatch.transient_max;
    put(&mut st, "u-1", u);
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Stuck);
    assert_eq!(u.transient_retries, ctx.cfg.dispatch.transient_max);
    assert!(exceptions(&ctx).contains("stuck u-1 error/host-down"));
    assert!(
        !host
            .logs()
            .iter()
            .any(|line| line.contains("transient-retry"))
    );
}

#[test]
fn a_real_failure_still_sticks() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"pushed unit/x-a1 abc\"",
    );
    host.reply("ls-remote", 0, "abc123 refs/heads/unit/x-a1");
    host.reply("task show", 0, "agent exit 1 after 10s");
    let mut st = State::default();
    let mut u = unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1"));
    u.attempts = 3;
    put(&mut st, "u-1", u);
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Stuck);
    assert_eq!(u.transient_retries, 0);
    assert!(exceptions(&ctx).contains("stuck u-1 finished/agent exit 1"));
}

#[test]
fn a_unit_unreported_for_under_90_seconds_is_left_alone() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected deadbeef bridge slot-9 finished \"someone else\"",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    assert_eq!(collect(&ctx, &mut st), None);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Inflight);
    assert!(!host.commands().iter().any(|c| c.contains("pool status")));
    assert!(!host.commands().iter().any(|c| c.contains("ls-remote")));
}

#[test]
fn an_unreported_unit_still_listed_by_pool_status_is_left_alone() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool status",
        0,
        "pool fly4 slot-1 1a2b3c4d running since 100\n",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1")),
    );
    host.advance(91);
    assert_eq!(collect(&ctx, &mut st), None);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Inflight);
    assert!(host.commands().iter().any(|c| c.contains("pool status")));
    assert!(!host.commands().iter().any(|c| c.contains("ls-remote")));
    assert!(!host.commands().iter().any(|c| c.contains("task show")));
}

#[test]
fn a_silent_failure_past_grace_renders_an_empty_reason_not_none() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected deadbeef bridge slot-9 finished \"someone else\"",
    );
    host.reply(
        "pool status",
        0,
        "pool fly4 slot-1 cafebabe running since 100\n",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u-1",
        unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/u-1-a1")),
    );
    host.advance(91);
    assert_eq!(collect(&ctx, &mut st), None);
    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Pending);
    let why = &u.history.last().unwrap().why;
    assert_eq!(why, "");
    assert!(!format!("{why:?}").contains("None"), "why is {why:?}");
}

#[test]
fn a_splitting_unit_with_no_answer_for_1500_seconds_is_stuck() {
    let (_dir, ctx, host) = setup_with(PumpConfig {
        planner: PlannerConfig {
            split_hard_cap: 1500,
            ..PlannerConfig::default()
        },
        ..PumpConfig::default()
    });
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    host.advance(1501);
    assert_eq!(collect(&ctx, &mut st), None);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Stuck);
    assert!(host.commands().is_empty());
    assert!(
        exceptions(&ctx)
            .contains("split-no-answer u-1 the split planner wrote no plan in 25 minutes")
    );
}

#[test]
fn a_splitting_unit_with_a_written_plan_is_left_to_ingest() {
    let (_dir, ctx, host) = setup_with(PumpConfig {
        planner: PlannerConfig {
            split_hard_cap: 1500,
            ..PlannerConfig::default()
        },
        ..PumpConfig::default()
    });
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/u-1.split.json"), "{}").unwrap();
    host.advance(1501);
    assert_eq!(collect(&ctx, &mut st), None);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Splitting);
    assert!(!exceptions(&ctx).contains("split-no-answer"));
}

#[test]
fn a_dead_split_planner_is_reported_and_retried() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    fs::create_dir_all(ctx.home.join("resplit")).unwrap();
    fs::write(ctx.home.join("resplit/u-1.out.rc"), "127\n").unwrap();

    assert_eq!(collect(&ctx, &mut st), None);

    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Splitting);
    assert_eq!(u.split_fails, 1);
    assert_eq!(u.split_retry_at, Some(T0 + 60));
    assert!(!ctx.home.join("resplit/u-1.out.rc").exists());
    assert!(
        exceptions(&ctx).contains(
            "split-planner-failed u-1 planner exited 127 without a plan; retry 1/3 in 60s"
        ),
        "{:?}",
        exceptions(&ctx)
    );
    assert!(host.spawned().is_empty());
}

#[test]
fn the_third_split_planner_failure_makes_the_unit_stuck() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    let mut splitting = unit(Status::Splitting, None, None);
    splitting.split_fails = 2;
    put(&mut st, "u-1", splitting);
    fs::create_dir_all(ctx.home.join("resplit")).unwrap();
    fs::write(ctx.home.join("resplit/u-1.out.rc"), "1\n").unwrap();

    assert_eq!(collect(&ctx, &mut st), None);

    assert_eq!(st.units.get("u-1").unwrap().status, Status::Stuck);
    assert!(
        exceptions(&ctx).contains(
            "split-planner-failed u-1 the split planner exited without a plan 3 times (last exit 1)"
        ),
        "{:?}",
        exceptions(&ctx)
    );
}

#[test]
fn spawn_planner_removes_a_stale_exit_receipt() {
    let (_dir, ctx, host) = setup_with(planner_cfg());
    let dir = ctx.home.join("resplit");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("u-9.out.rc"), "127\n").unwrap();

    spawn_planner(&ctx, "resplit", "u-9", "p");

    assert!(!ctx.home.join("resplit/u-9.out.rc").exists());
    assert_eq!(
        host.spawned(),
        vec![String::from("plan-it /charter/path.md --quiet")]
    );
}

/// Write the split planner's raw output for `uid`.
fn write_planner_out(ctx: &Ctx, uid: &str, text: &str) {
    let dir = ctx.home.join("resplit");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{uid}.out")), text).unwrap();
}

#[test]
fn a_split_planner_that_died_host_class_returns_the_unit_pending() {
    let (_dir, ctx, host) = setup_with(PumpConfig {
        planner: PlannerConfig {
            split_hard_cap: 1500,
            ..PlannerConfig::default()
        },
        ..PumpConfig::default()
    });
    let mut st = State::default();
    let mut splitting = unit(Status::Splitting, None, None);
    splitting.attempts = 1;
    splitting.history.push(Attempt {
        attempt: 1,
        why: String::from("timeout"),
        at: T0,
        ..Attempt::default()
    });
    put(&mut st, "u-1", splitting);
    write_planner_out(&ctx, "u-1", "bash: line 1: pi: command not found\n");
    host.advance(60);

    assert_eq!(collect(&ctx, &mut st), None);

    let stuck = st.units.get("u-1").unwrap();
    assert_eq!(stuck.status, Status::Pending);
    assert!(stuck.task.is_none());
    assert!(stuck.since > T0, "the unit restarts its pending clock");
    let lines: Vec<String> = exceptions(&ctx)
        .lines()
        .filter(|line| line.contains("environment u-1"))
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 1, "{:?}", exceptions(&ctx));
    assert!(
        lines[0].contains("split planner died: bash: line 1: pi: command not found"),
        "{}",
        lines[0]
    );
}

#[test]
fn a_silent_split_planner_still_goes_stuck() {
    let (_dir, ctx, host) = setup_with(PumpConfig {
        planner: PlannerConfig {
            split_hard_cap: 1500,
            ..PlannerConfig::default()
        },
        ..PumpConfig::default()
    });
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    write_planner_out(&ctx, "u-1", "thinking...\n");

    host.advance(1501);
    assert_eq!(collect(&ctx, &mut st), None);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Stuck);
    assert!(
        exceptions(&ctx)
            .contains("split-no-answer u-1 the split planner wrote no plan in 25 minutes"),
        "{:?}",
        exceptions(&ctx)
    );
}

#[test]
fn a_finished_gate_task_returns_its_receipt_text() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected gate9 bridge slot-2 finished \"gate done\"",
    );
    host.reply("task show", 0, "GATE RECEIPT");
    let mut st = State {
        last_gate: GateRun {
            at: T0,
            task: Some(String::from("gate9")),
            ..GateRun::default()
        },
        ..State::default()
    };
    assert_eq!(collect(&ctx, &mut st), Some(String::from("GATE RECEIPT")));
}

#[test]
fn spawn_planner_writes_the_prompt_and_spawns_with_the_charter_substituted() {
    let (dir, ctx, host) = setup_with(planner_cfg());
    spawn_planner(&ctx, "resplit", "u-9", "prompt body");
    assert_eq!(
        fs::read_to_string(dir.path().join("home/resplit/u-9.prompt")).unwrap(),
        "prompt body"
    );
    assert_eq!(
        host.spawned(),
        vec![String::from("plan-it /charter/path.md --quiet")]
    );

    let dir2 = tempdir().unwrap();
    let mut cfg = planner_cfg();
    cfg.shell_prefix = String::from("source env.sh");
    let host2 = FakeHost::new();
    let ctx2 = Ctx::with_parts(
        cfg,
        host2.clone(),
        dir2.path().join("project"),
        String::from("vivi"),
        String::from("source env.sh"),
        dir2.path().join("home"),
        dir2.path().join("home/repo"),
    );
    spawn_planner(&ctx2, "plans", "u-9", "p2");
    assert_eq!(
        host2.spawned(),
        vec![String::from(
            "source env.sh; plan-it /charter/path.md --quiet"
        )]
    );
}

#[test]
fn an_agent_exit_127_with_a_branch_is_a_host_failure_not_an_attempt() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d here slot-1 finished \"pushed unit/x-a1 abc\"",
    );
    host.reply("ls-remote", 0, "abc123\trefs/heads/unit/x-a1\n");
    host.reply(
        "task show",
        0,
        "agent exit 127 in 1s, job 12s, started from abc\n\
         \n\
         --- stderr tail\n\
         bash: line 1: pi: command not found",
    );
    let mut st = State::default();
    let mut u = unit(Status::Inflight, Some("1a2b3c4d"), Some("unit/x-a1"));
    u.attempts = 1;
    u.pool = Some(String::from("here"));
    put(&mut st, "u-1", u);

    assert_eq!(collect(&ctx, &mut st), None);

    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Pending);
    assert_eq!(u.attempts, 0);
    assert!(u.history.is_empty());
    assert_eq!(u.task, None);
    assert_eq!(u.branch, None);
    assert_eq!(u.since_dispatch, None);
    assert!(
        host.commands().iter().any(
            |command| command.contains("push origin --delete") || command.contains("branch -D")
        ),
        "commands: {:?}",
        host.commands()
    );
    let pause = &st.env_pauses["here"];
    assert_eq!(pause.fails.len(), 1);
    assert_eq!(pause.paused_at, None);
    assert!(exceptions(&ctx).is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("host-fail u-1 agent_exit; attempt restored")),
        "logs: {:?}",
        host.logs()
    );
}

#[test]
fn a_checkout_failed_result_is_a_host_failure_that_restores_the_attempt() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 finished \"checkout_failed\"",
    );
    host.reply("ls-remote", 0, "");
    host.reply(
        "task show",
        0,
        "agent exit 1 in 2s, job 5s\n\
         \n\
         --- runner log tail\n\
         Unable to create '/repo/.git/index.lock': File exists.",
    );
    let mut st = State::default();
    let mut u = unit(Status::Inflight, Some("1a2b3c4d"), None);
    u.attempts = 2;
    u.pool = Some(String::from("here"));
    put(&mut st, "u-1", u);

    assert_eq!(collect(&ctx, &mut st), None);

    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Pending);
    assert_eq!(u.attempts, 1);
    assert!(u.history.is_empty());
    assert_eq!(u.task, None);
    let pause = &st.env_pauses["here"];
    assert_eq!(pause.fails.len(), 1);
    assert_eq!(pause.paused_at, None);
    assert!(exceptions(&ctx).is_empty());
}

#[test]
fn two_host_failures_pause_the_pool_with_one_environment_exception() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected task-1 here slot-1 finished \"pushed nothing\"\n\
         collected task-2 here slot-2 finished \"pushed nothing\"",
    );
    host.reply("ls-remote", 0, "");
    host.reply(
        "task show",
        0,
        "agent exit 127 in 1s, job 12s\n\
         \n\
         --- stderr tail\n\
         bash: line 1: pi: command not found",
    );
    let mut st = State::default();
    let mut u1 = unit(Status::Inflight, Some("task-1"), None);
    u1.pool = Some(String::from("here"));
    let mut u2 = unit(Status::Inflight, Some("task-2"), None);
    u2.pool = Some(String::from("here"));
    put(&mut st, "u-1", u1);
    put(&mut st, "u-2", u2);

    assert_eq!(collect(&ctx, &mut st), None);

    assert!(st.units.values().all(|u| u.status == Status::Pending));
    let pause = &st.env_pauses["here"];
    assert_eq!(pause.fails.len(), 2);
    assert_eq!(pause.paused_at, Some(T0));
    let recorded = exceptions(&ctx);
    let lines: Vec<&str> = recorded
        .lines()
        .filter(|line| line.contains("environment here"))
        .collect();
    assert_eq!(lines.len(), 1, "exceptions: {recorded}");
    assert!(lines[0].contains("pi"), "exceptions: {recorded}");
}
