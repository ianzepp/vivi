//! Tests for [`crate::pump::collect`]: the reconcile parser and the
//! in-flight unit state machine, driven through [`FakeHost`] replies.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::collect::burst::RATE_LIMIT_EXCEPTION_WINDOW;
use crate::pump::collect::*;
use crate::pump::config::{PlannerConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::state::{Attempt, State, Status, Unit};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

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

/// A due split retry at the planner cap staggers instead of enqueueing: the
/// unit stays `Splitting` with a moved-out retry stamp; existing jobs remain.
#[test]
fn a_due_split_retry_defers_when_the_planner_cap_is_full() {
    let (_dir, ctx, host) = setup_with(PumpConfig {
        planner: PlannerConfig {
            run: String::from("plan-it {charter} --quiet"),
            charter: String::from("/charter/path.md"),
            split_hard_cap: 1500,
            planners_max: 1,
            ..PlannerConfig::default()
        },
        ..PumpConfig::default()
    });
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    let mut u2 = unit(Status::Splitting, None, None);
    u2.split_retry_at = Some(T0);
    put(&mut st, "u-2", u2);

    collect(&ctx, &mut st);

    assert!(host.spawned().is_empty());
    let u = st.units.get("u-2").unwrap();
    assert_eq!(u.status, Status::Splitting);
    assert_eq!(u.split_retry_at, Some(T0 + 60));
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("split-launch-deferred u-2")),
        "{:?}",
        host.logs()
    );
}

#[test]
fn a_receipt_less_splitting_unit_is_stuck_at_the_hard_cap() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    host.advance(5401);
    collect(&ctx, &mut st);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Stuck);
    assert!(host.commands().is_empty());
    assert!(
        exceptions(&ctx).contains(
            "split-no-answer u-1 the split planner is still unreachable after 90 minutes"
        )
    );
}

#[test]
fn a_live_split_planner_past_split_timeout_is_not_stuck() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    host.advance(2250);
    collect(&ctx, &mut st);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Splitting);
    assert!(!exceptions(&ctx).contains("split-no-answer"));
    assert!(!exceptions(&ctx).contains("split-planner-failed"));
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
    collect(&ctx, &mut st);
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

    collect(&ctx, &mut st);

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

    host.advance(61);
    collect(&ctx, &mut st);

    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Splitting);
    assert_eq!(u.since, T0 + 61);
    assert_eq!(u.split_retry_at, None);
    let retry_id = crate::pump::planner_jobs::job_id("split", "u-1", T0 + 61);
    assert_eq!(
        st.jobs[&retry_id].status,
        crate::pump::jobs::JobStatus::Pending
    );
    assert!(host.spawned().is_empty());
}

/// The exact provider 429 line a rate-limited split planner leaves behind.
const RATE_LIMITED_OUT: &str =
    "429: {\"code\":\"1302\",\"message\":\"Rate limit reached for requests\"}";

/// Write a rate-limited planner receipt: the 429 output plus a nonzero exit.
fn rate_limited_receipt(ctx: &Ctx, uid: &str) {
    let dir = ctx.home.join("resplit");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{uid}.out")), RATE_LIMITED_OUT).unwrap();
    fs::write(dir.join(format!("{uid}.out.rc")), "1\n").unwrap();
}

#[test]
fn a_rate_limited_split_planner_keeps_its_retry_budget() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    rate_limited_receipt(&ctx, "u-1");

    collect(&ctx, &mut st);

    let u = st.units.get("u-1").unwrap();
    assert_eq!(u.status, Status::Splitting);
    assert_eq!(u.split_fails, 0);
    let at = u.split_retry_at.expect("a rate-limited planner is retried");
    assert!(at >= T0 + 120, "retry at {at}");
    assert!(!ctx.home.join("resplit/u-1.out.rc").exists());
    assert!(
        !exceptions(&ctx).contains("split-planner-failed"),
        "{:?}",
        exceptions(&ctx)
    );
    assert!(host.spawned().is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("split-rate-limited u-1 retry in")),
        "{:?}",
        host.logs()
    );

    host.advance(135);
    collect(&ctx, &mut st);

    let retry_id = crate::pump::planner_jobs::job_id("split", "u-1", ctx.now());
    assert_eq!(
        st.jobs[&retry_id].status,
        crate::pump::jobs::JobStatus::Pending
    );
    assert!(host.spawned().is_empty());
    assert_eq!(st.units.get("u-1").unwrap().split_fails, 0);
}

#[test]
fn repeated_rate_limits_back_off_without_parking() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    let mut last: Option<u64> = None;
    for round in 0..3 {
        rate_limited_receipt(&ctx, "u-1");
        collect(&ctx, &mut st);
        let u = st.units.get("u-1").unwrap();
        assert_eq!(u.status, Status::Splitting, "round {round}");
        assert_eq!(u.split_fails, 0, "round {round}");
        let at = u.split_retry_at.unwrap_or_default();
        if let Some(previous) = last {
            assert!(at > previous, "round {round}: {at} not after {previous}");
        }
        last = Some(at);
        host.advance(600);
    }
    assert!(
        !exceptions(&ctx).contains("split-planner-failed"),
        "{:?}",
        exceptions(&ctx)
    );
}

#[test]
fn a_split_rate_limit_burst_raises_one_exception_per_window() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    put(&mut st, "u-1", unit(Status::Splitting, None, None));
    put(&mut st, "u-2", unit(Status::Splitting, None, None));
    for uid in ["u-1", "u-2"] {
        rate_limited_receipt(&ctx, uid);
    }

    collect(&ctx, &mut st);
    assert_eq!(
        exceptions(&ctx)
            .lines()
            .filter(|line| line.contains("provider-rate-limit"))
            .count(),
        1
    );
    for uid in ["u-1", "u-2"] {
        rate_limited_receipt(&ctx, uid);
    }
    host.advance(100);
    collect(&ctx, &mut st);
    assert_eq!(
        exceptions(&ctx)
            .lines()
            .filter(|line| line.contains("provider-rate-limit"))
            .count(),
        1
    );
    for uid in ["u-1", "u-2"] {
        rate_limited_receipt(&ctx, uid);
    }
    host.advance(RATE_LIMIT_EXCEPTION_WINDOW - 100 + 1);
    collect(&ctx, &mut st);
    assert_eq!(
        exceptions(&ctx)
            .lines()
            .filter(|line| line.contains("provider-rate-limit"))
            .count(),
        2
    );
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

    collect(&ctx, &mut st);

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
fn split_planner_uses_durable_job_and_clears_stale_receipt() {
    let (dir, ctx, host) = setup_with(PumpConfig {
        planner: PlannerConfig {
            split_hard_cap: 1500,
            ..PlannerConfig::default()
        },
        ..PumpConfig::default()
    });
    let ctx = ctx.with_routing(RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("pump-planner"),
            kind: Some(String::from("planner")),
            status: String::from("active"),
            pool: Some(String::from("planner-pool")),
            read_only: Some(true),
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::from([(
            String::from("planner-pool"),
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
    let output = ctx.home.join("resplit/u-9.out");
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, "old output").unwrap();
    fs::write(crate::pump::host::exit_receipt_path(&output), "127\n").unwrap();
    let mut st = State::default();

    let id = crate::pump::planner_jobs::enqueue(
        &ctx,
        &mut st,
        "split",
        "u-9",
        T0,
        "split input",
        ctx.cfg.planner.split_hard_cap,
        "resplit/u-9.out",
        "plans/u-9.split.json",
        Some("default"),
    )
    .unwrap();

    let job = &st.jobs[&id];
    assert_eq!(job.kind, "planner:split");
    assert_eq!(job.pool_candidates, vec![String::from("planner-pool")]);
    assert_eq!(job.status, crate::pump::jobs::JobStatus::Pending);
    assert_eq!(job.target.as_deref(), Some("default"));
    assert_eq!(job.budget, 1500);
    assert_eq!(fs::read_to_string(output).unwrap(), "");
    assert!(!crate::pump::host::exit_receipt_path(&ctx.home.join("resplit/u-9.out")).exists());
    assert!(job.body.contains("split input"));
    assert!(job.body.contains("PUMP-PLAN-BEGIN"));
    assert!(host.spawned().is_empty());
    assert!(dir.path().join("home/resplit/u-9.out").exists());
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

    collect(&ctx, &mut st);

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
    collect(&ctx, &mut st);
    assert_eq!(st.units.get("u-1").unwrap().status, Status::Stuck);
    assert!(
        exceptions(&ctx).contains(
            "split-no-answer u-1 the split planner is still unreachable after 25 minutes"
        ),
        "{:?}",
        exceptions(&ctx)
    );
}
