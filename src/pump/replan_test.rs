//! Tests for operator replans, from queued request through board re-planning.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use tempfile::{TempDir, tempdir};

use crate::cli::PumpReplanCommand;
use crate::pump::audit::{AuditRun, current as current_audit, store as store_audit};
use crate::pump::config::PumpConfig;
use crate::pump::control::project_home as resolve_project_home;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::replan::{apply_pending, run};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore, ReplanRequest};

const T0: u64 = 1_000_000;

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    (dir, ctx, host)
}

fn request(ctx: &Ctx, need: &str, force: bool) {
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.add_replan_request(need, force, T0).unwrap();
}

fn need(st: &mut State, handle: &str, verdict: &str, units: &[(&str, Status)]) {
    let ids = units.iter().map(|(uid, _)| (*uid).to_owned()).collect();
    st.needs.insert(
        handle.to_owned(),
        NeedRecord {
            verdict: verdict.to_owned(),
            units: ids,
            ..NeedRecord::default()
        },
    );
    for (uid, status) in units {
        st.units.insert(
            (*uid).to_owned(),
            Unit {
                need: handle.to_owned(),
                status: *status,
                ..Unit::default()
            },
        );
    }
}

#[test]
fn replan_apply_supersedes_the_plan_and_forgets_the_need() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);
    st.plans_seen.push(String::from("n1.json"));
    st.board.replied = vec![String::from("n1:planned"), String::from("n1:merged")];
    st.board.planner_failed.push(String::from("n1"));
    st.board.planner_fails.insert(String::from("n1"), 3);
    st.board
        .planner_retry_at
        .insert(String::from("n1"), T0 + 30);
    st.board.planner_pids.insert(String::from("n1"), 42);
    st.board.revision_planning.insert(String::from("n1"), T0);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/n1.json"), "old plan").unwrap();
    request(&ctx, "n1", false);

    apply_pending(&ctx, &mut st);

    assert!(!ctx.home.join("plans/n1.json").exists());
    assert!(
        ctx.home
            .join(format!("plans/superseded/n1.{T0}.json"))
            .exists()
    );
    assert!(!st.plans_seen.contains(&String::from("n1.json")));
    assert!(!st.needs.contains_key("n1"));
    assert!(!st.board.replied.iter().any(|key| key.starts_with("n1:")));
    assert!(!st.board.planner_failed.contains(&String::from("n1")));
    assert!(!st.board.planner_fails.contains_key("n1"));
    assert!(!st.board.planner_retry_at.contains_key("n1"));
    assert!(!st.board.planner_pids.contains_key("n1"));
    assert!(!st.board.revision_planning.contains_key("n1"));
    assert_eq!(st.board.replan, vec![String::from("n1")]);
}

#[test]
fn replan_apply_withdraws_pending_and_leaves_merged() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(
        &mut st,
        "n1",
        "units",
        &[
            ("u1", Status::Pending),
            ("u2", Status::Held),
            ("u3", Status::Stuck),
            ("u4", Status::NeedsReplan),
            ("u5", Status::Merged),
        ],
    );
    request(&ctx, "n1", false);

    apply_pending(&ctx, &mut st);

    assert_eq!(st.units["u1"].status, Status::Withdrawn);
    assert_eq!(st.units["u2"].status, Status::Withdrawn);
    assert_eq!(st.units["u3"].status, Status::Withdrawn);
    assert_eq!(st.units["u4"].status, Status::Withdrawn);
    assert_eq!(st.units["u5"].status, Status::Merged);
}

#[test]
fn replan_apply_consumes_the_request() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);
    request(&ctx, "n1", false);
    apply_pending(&ctx, &mut st);
    let logs = host.logs().len();

    apply_pending(&ctx, &mut st);

    assert_eq!(host.logs().len(), logs);
    assert!(!st.needs.contains_key("n1"));
    assert!(pending_in(&ctx.home).is_empty());
}

#[test]
fn replan_apply_skips_unknown_need() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    request(&ctx, "missing", false);

    apply_pending(&ctx, &mut st);

    assert!(
        host.logs()
            .iter()
            .any(|line| { line == "replan: missing unknown; nothing to supersede" })
    );
    assert!(pending_in(&ctx.home).is_empty());
}

#[test]
fn replan_apply_tolerates_an_absent_plan_file() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);
    request(&ctx, "n1", false);

    apply_pending(&ctx, &mut st);

    assert!(!st.needs.contains_key("n1"));
    assert_eq!(st.board.replan, vec![String::from("n1")]);
    assert!(pending_in(&ctx.home).is_empty());
}

#[test]
fn replan_apply_keeps_state_and_request_when_plan_cannot_move() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);
    st.plans_seen.push(String::from("n1.json"));
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/n1.json"), "old plan").unwrap();
    fs::write(
        ctx.home.join("plans/superseded"),
        "blocks directory creation",
    )
    .unwrap();
    request(&ctx, "n1", false);

    apply_pending(&ctx, &mut st);

    assert!(st.needs.contains_key("n1"));
    assert_eq!(st.plans_seen, vec![String::from("n1.json")]);
    assert!(ctx.home.join("plans/n1.json").is_file());
    assert_eq!(pending_in(&ctx.home).len(), 1);
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("request remains pending"))
    );

    fs::remove_file(ctx.home.join("plans/superseded")).unwrap();
    apply_pending(&ctx, &mut st);
    assert!(!st.needs.contains_key("n1"));
    assert!(
        ctx.home
            .join(format!("plans/superseded/n1.{T0}.json"))
            .exists()
    );
    assert!(pending_in(&ctx.home).is_empty());
}

#[test]
fn replan_apply_persists_state_before_request_marker() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/n1.json"), "old plan").unwrap();
    request(&ctx, "n1", false);
    let path = ctx.home.join(DB_FILENAME);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_replan_mark BEFORE UPDATE OF applied_at ON replan_requests
             BEGIN SELECT RAISE(ABORT, 'simulated request marker failure'); END;",
        )
        .unwrap();

    apply_pending(&ctx, &mut st);

    let mut durable = PumpStore::open_read_only(&path).unwrap().unwrap();
    let saved = durable.load().unwrap();
    assert!(!saved.needs.contains_key("n1"));
    assert!(saved.board.replan.contains(&String::from("n1")));
    assert_eq!(durable.pending_replan_requests().unwrap().len(), 1);
    drop(durable);
    connection
        .execute_batch("DROP TRIGGER fail_replan_mark;")
        .unwrap();

    let mut reloaded = PumpStore::open_read_only(&path)
        .unwrap()
        .unwrap()
        .load()
        .unwrap();
    apply_pending(&ctx, &mut reloaded);

    assert!(!reloaded.needs.contains_key("n1"));
    assert!(reloaded.board.replan.contains(&String::from("n1")));
    assert!(pending_in(&ctx.home).is_empty());
}

#[test]
fn replan_refus_live_unit_without_force_changes_nothing() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "units", &[("u1", Status::Inflight)]);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/n1.json"), "old plan").unwrap();
    request(&ctx, "n1", false);

    apply_pending(&ctx, &mut st);

    assert!(st.needs.contains_key("n1"));
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert!(ctx.home.join("plans/n1.json").is_file());
    let exception = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exception.contains("replan-refused n1"), "{exception}");
    assert!(
        exception.contains("use vivi pump replan n1 --force"),
        "{exception}"
    );
    assert!(pending_in(&ctx.home).is_empty());
}

#[test]
fn replan_refus_while_planner_runs() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);
    st.board.planning.insert(String::from("n1"), T0);
    request(&ctx, "n1", false);

    apply_pending(&ctx, &mut st);

    assert!(st.needs.contains_key("n1"));
    assert!(st.board.planning.contains_key("n1"));
    let exception = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exception.contains("a planner is already running for this need"));
    assert!(pending_in(&ctx.home).is_empty());
}

#[test]
fn replan_force_applies_over_live_work() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "units", &[("u1", Status::Inflight)]);
    request(&ctx, "n1", true);

    apply_pending(&ctx, &mut st);

    assert!(!st.needs.contains_key("n1"));
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert_eq!(st.board.replan, vec![String::from("n1")]);
    assert!(pending_in(&ctx.home).is_empty());
}

fn mailspace_project() -> TempDir {
    let dir = tempdir().unwrap();
    let vivi = dir.path().join(".vivi");
    fs::create_dir_all(&vivi).unwrap();
    fs::write(
        vivi.join("mailspace.toml"),
        "name = \"replan\"\n\n[pump]\nbranch = \"next\"\n",
    )
    .unwrap();
    dir
}

fn project_home(dir: &TempDir) -> PathBuf {
    resolve_project_home(dir.path()).unwrap()
}

fn seed_need(dir: &TempDir, handle: &str, verdict: &str, status: Option<Status>) {
    let units = status
        .map(|_| vec![format!("{handle}-1")])
        .unwrap_or_default();
    let mut st = State::default();
    st.needs.insert(
        handle.to_owned(),
        NeedRecord {
            verdict: verdict.to_owned(),
            units,
            ..NeedRecord::default()
        },
    );
    if let Some(status) = status {
        st.units.insert(
            format!("{handle}-1"),
            Unit {
                need: handle.to_owned(),
                status,
                ..Unit::default()
            },
        );
    }
    let mut store = PumpStore::open(&project_home(dir).join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();
}

fn command(need: &str, force: bool) -> PumpReplanCommand {
    PumpReplanCommand {
        need: need.to_owned(),
        force,
        project: None,
    }
}

fn pending_in(home: &Path) -> Vec<ReplanRequest> {
    PumpStore::open_read_only(&home.join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .pending_replan_requests()
        .unwrap()
}

fn apply_ctx(dir: &TempDir) -> (Ctx, Arc<FakeHost>) {
    let (_temp, mut ctx, host) = setup();
    ctx.home = project_home(dir);
    (ctx, host)
}

#[test]
fn run_replan_writes_a_request_row() {
    let dir = mailspace_project();
    seed_need(&dir, "n1", "units", Some(Status::Pending));

    run(dir.path(), &command("n1", false)).unwrap();

    let rows = pending_in(&project_home(&dir));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].need, "n1");
    assert!(!rows[0].force);
    let (ctx, _host) = apply_ctx(&dir);
    let mut st = PumpStore::open_read_only(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .load()
        .unwrap();
    apply_pending(&ctx, &mut st);
    assert!(!st.needs.contains_key("n1"));
}

#[test]
fn explicit_replan_rearms_an_exhausted_gatefix_audit_without_a_need_record() {
    let dir = mailspace_project();
    let need = "pumpfix-g01234567";
    let mut st = State::default();
    store_audit(
        &mut st,
        Some(AuditRun {
            need: need.to_owned(),
            tip: String::from("0123456789abcdef0123456789abcdef01234567"),
            fails: 3,
            attempts: 3,
            exhausted: true,
            last_error: Some(String::from("provider: Connection error.")),
            ..AuditRun::default()
        }),
    );
    let mut store = PumpStore::open(&project_home(&dir).join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();

    run(dir.path(), &command(need, false)).unwrap();
    let (ctx, _host) = apply_ctx(&dir);
    let mut st = PumpStore::open_read_only(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .load()
        .unwrap();
    apply_pending(&ctx, &mut st);

    let audit = current_audit(&st).unwrap();
    assert!(!audit.exhausted);
    assert_eq!(audit.fails, 0);
    assert_eq!(audit.attempts, 3);
    assert_eq!(audit.retry_at, Some(T0));
    assert!(
        audit
            .last_error
            .unwrap()
            .contains("operator replan requested")
    );
    assert!(pending_in(&ctx.home).is_empty());
    let durable = PumpStore::open_read_only(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .load()
        .unwrap();
    assert!(!current_audit(&durable).unwrap().exhausted);
}

#[test]
fn run_replan_accepts_an_invalid_verdict() {
    let dir = mailspace_project();
    seed_need(&dir, "n1", "invalid", None);

    run(dir.path(), &command("n1", false)).unwrap();

    assert_eq!(pending_in(&project_home(&dir))[0].need, "n1");
}

#[test]
fn run_replan_rejects_unknown_need() {
    let dir = mailspace_project();
    seed_need(&dir, "known", "invalid", None);

    let error = run(dir.path(), &command("missing", false)).unwrap_err();

    assert!(error.to_string().contains("unknown need: missing"));
    assert!(pending_in(&project_home(&dir)).is_empty());
}

#[test]
fn run_replan_refuses_live_work_and_honors_force() {
    let dir = mailspace_project();
    seed_need(&dir, "n1", "units", Some(Status::Inflight));

    let error = run(dir.path(), &command("n1", false)).unwrap_err();
    assert!(error.to_string().contains("unit n1-1 is inflight"));
    assert!(error.to_string().contains("--force"));
    assert!(pending_in(&project_home(&dir)).is_empty());

    run(dir.path(), &command("n1", true)).unwrap();

    let rows = pending_in(&project_home(&dir));
    assert_eq!(rows.len(), 1);
    assert!(rows[0].force);
}

#[test]
fn run_replan_refuses_merging_work() {
    let dir = mailspace_project();
    seed_need(&dir, "n1", "units", Some(Status::Merging));

    let error = run(dir.path(), &command("n1", false)).unwrap_err();

    assert!(error.to_string().contains("unit n1-1 is merging"));
    assert!(error.to_string().contains("--force"));
    assert!(pending_in(&project_home(&dir)).is_empty());
}

#[test]
fn run_replan_without_a_pump_database_is_refused() {
    let dir = mailspace_project();

    let error = run(dir.path(), &command("n1", false)).unwrap_err();

    assert!(error.to_string().contains("no pump.db at"));
}
