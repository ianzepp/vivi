//! Tests for operator replans: the state change the pump applies, and the CLI
//! half that posts the action on the need's open pump record.

use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::cli::PumpReplanCommand;
use crate::mailspace::PumpAction;
use crate::pump::config::PumpConfig;
use crate::pump::fixture::{Project, project};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::replan::{Outcome, apply_one, run};
use crate::pump::state::{NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

const T0: u64 = 1_000_000;

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    (dir, ctx, host)
}

/// Apply a replan of `need` the way the pump's tick would.
fn replan(ctx: &Ctx, st: &mut State, need: &str, force: bool) -> Outcome {
    apply_one(ctx, st, need, force).unwrap()
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

    assert_eq!(replan(&ctx, &mut st, "n1", false), Outcome::Superseded);

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

    replan(&ctx, &mut st, "n1", false);

    assert_eq!(st.units["u1"].status, Status::Withdrawn);
    assert_eq!(st.units["u2"].status, Status::Withdrawn);
    assert_eq!(st.units["u3"].status, Status::Withdrawn);
    assert_eq!(st.units["u4"].status, Status::Withdrawn);
    assert_eq!(st.units["u5"].status, Status::Merged);
}

#[test]
fn replaying_an_applied_replan_is_harmless() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);
    assert_eq!(replan(&ctx, &mut st, "n1", false), Outcome::Superseded);

    assert_eq!(replan(&ctx, &mut st, "n1", false), Outcome::Superseded);

    assert!(!st.needs.contains_key("n1"));
}

#[test]
fn a_need_with_no_plan_still_has_its_parked_planner_cleared() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    st.board.planner_failed.push(String::from("n2"));
    st.board.planner_fails.insert(String::from("n2"), 3);

    assert_eq!(replan(&ctx, &mut st, "n2", false), Outcome::Superseded);

    assert!(st.board.planner_failed.is_empty());
    assert!(st.board.planner_fails.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|line| line == "replan: n2 superseded by operator request")
    );
}

#[test]
fn replan_apply_tolerates_an_absent_plan_file() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);

    replan(&ctx, &mut st, "n1", false);

    assert!(!st.needs.contains_key("n1"));
}

#[test]
fn replan_apply_keeps_the_state_when_the_plan_cannot_move() {
    let (_dir, ctx, _host) = setup();
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

    assert!(apply_one(&ctx, &mut st, "n1", false).is_err());

    assert!(st.needs.contains_key("n1"));
    assert_eq!(st.plans_seen, vec![String::from("n1.json")]);
    assert!(ctx.home.join("plans/n1.json").is_file());

    fs::remove_file(ctx.home.join("plans/superseded")).unwrap();
    assert_eq!(replan(&ctx, &mut st, "n1", false), Outcome::Superseded);
    assert!(!st.needs.contains_key("n1"));
    assert!(
        ctx.home
            .join(format!("plans/superseded/n1.{T0}.json"))
            .exists()
    );
}

#[test]
fn replan_refus_live_unit_without_force_changes_nothing() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "units", &[("u1", Status::Inflight)]);
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join("plans/n1.json"), "old plan").unwrap();

    assert_eq!(replan(&ctx, &mut st, "n1", false), Outcome::Refused);

    assert!(st.needs.contains_key("n1"));
    assert_eq!(st.units["u1"].status, Status::Inflight);
    assert!(ctx.home.join("plans/n1.json").is_file());
    let exception = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exception.contains("replan-refused n1"), "{exception}");
    assert!(
        exception.contains("use vivi pump replan n1 --force"),
        "{exception}"
    );
}

#[test]
fn replan_refus_while_planner_runs() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "invalid", &[]);
    st.board.planning.insert(String::from("n1"), T0);

    assert_eq!(replan(&ctx, &mut st, "n1", false), Outcome::Refused);

    assert!(st.needs.contains_key("n1"));
    assert!(st.board.planning.contains_key("n1"));
    let exception = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exception.contains("a planner is already running for this need"));
}

#[test]
fn replan_force_applies_over_live_work() {
    let (_dir, ctx, _host) = setup();
    let mut st = State::default();
    need(&mut st, "n1", "units", &[("u1", Status::Inflight)]);

    assert_eq!(replan(&ctx, &mut st, "n1", true), Outcome::Superseded);

    assert!(!st.needs.contains_key("n1"));
    assert_eq!(st.units["u1"].status, Status::Inflight);
}

/// Seed the pump's saved state with `handle` planned into one unit.
fn seed_need(project: &Project, handle: &str, verdict: &str, status: Option<Status>) {
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
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();
}

fn command(need: &str, force: bool) -> PumpReplanCommand {
    PumpReplanCommand {
        need: need.to_owned(),
        force,
        project: None,
    }
}

/// The actions posted and not yet applied.
fn posted(project: &Project) -> Vec<PumpAction> {
    project
        .mailspace
        .pump_pending_actions(&BTreeSet::new())
        .unwrap()
}

/// The pump's saved state.
fn saved(project: &Project) -> State {
    PumpStore::open_read_only(&project.home().join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .load()
        .unwrap()
}

#[test]
fn run_replan_posts_an_action_on_the_open_record_and_the_tick_applies_it() {
    let project = project();
    let need = project.need("fix auth");
    let record = project.mailspace.pump_send(None, &need, None).unwrap();
    seed_need(&project, &need, "units", Some(Status::Pending));

    run(project.root(), &command(&need, false)).unwrap();

    let actions = posted(&project);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].record, record.handle);
    assert!(!actions[0].request.force);
    let ctx = project.pump_ctx();
    let mut st = saved(&project);
    crate::pump::action::apply_pending(&ctx, &mut st);
    assert!(!st.needs.contains_key(&need));
    assert_eq!(project.mailspace.pump_history(&need).unwrap().len(), 2);
}

#[test]
fn run_replan_without_an_open_record_is_refused_and_says_how_to_open_one() {
    let project = project();
    let need = project.need("fix auth");
    seed_need(&project, &need, "units", Some(Status::Pending));

    let error = run(project.root(), &command(&need, false)).unwrap_err();

    let text = error.to_string();
    assert!(text.contains("no open pump record"), "{text}");
    assert!(
        text.contains(&format!("vivi pump send --need {need}")),
        "{text}"
    );
    assert!(posted(&project).is_empty());
}

#[test]
fn run_replan_accepts_an_invalid_verdict_and_a_parked_planner() {
    let project = project();
    let need = project.need("fix auth");
    let parked = project.need("fix billing");
    project.mailspace.pump_send(None, &need, None).unwrap();
    project.mailspace.pump_send(None, &parked, None).unwrap();
    seed_need(&project, &need, "invalid", None);
    let mut st = saved(&project);
    st.board.planner_failed.push(parked.clone());
    PumpStore::open(&project.home().join(DB_FILENAME))
        .unwrap()
        .save(&st)
        .unwrap();

    run(project.root(), &command(&need, false)).unwrap();
    run(project.root(), &command(&parked, false)).unwrap();

    assert_eq!(posted(&project).len(), 2);
}

#[test]
fn run_replan_rejects_unknown_need() {
    let project = project();
    let known = project.need("fix auth");
    let other = project.need("fix billing");
    project.mailspace.pump_send(None, &other, None).unwrap();
    seed_need(&project, &known, "invalid", None);

    let error = run(project.root(), &command(&other, false)).unwrap_err();

    assert!(
        error
            .to_string()
            .contains(&format!("unknown need: {other}"))
    );
    assert!(posted(&project).is_empty());
}

#[test]
fn run_replan_refuses_live_work_and_honors_force() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    seed_need(&project, &need, "units", Some(Status::Inflight));

    let error = run(project.root(), &command(&need, false)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("unit {need}-1 is inflight"))
    );
    assert!(error.to_string().contains("--force"));
    assert!(posted(&project).is_empty());

    run(project.root(), &command(&need, true)).unwrap();

    let actions = posted(&project);
    assert_eq!(actions.len(), 1);
    assert!(actions[0].request.force);
}

#[test]
fn run_replan_refuses_merging_work() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    seed_need(&project, &need, "units", Some(Status::Merging));

    let error = run(project.root(), &command(&need, false)).unwrap_err();

    assert!(
        error
            .to_string()
            .contains(&format!("unit {need}-1 is merging"))
    );
    assert!(error.to_string().contains("--force"));
    assert!(posted(&project).is_empty());
}

#[test]
fn run_replan_without_a_pump_database_is_refused() {
    let project = project();

    let error = run(project.root(), &command("n1", false)).unwrap_err();

    assert!(error.to_string().contains("no pump.db at"));
}
