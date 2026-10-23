//! Tests for [`crate::pump::requeue`]: requeue requests applied to state,
//! with the attempt counter kept clear of branches that already exist, and
//! the CLI half that posts them as actions on a need's open pump record.

use std::collections::BTreeSet;
use std::sync::Arc;

use tempfile::tempdir;

use crate::cli::PumpRequeueCommand;
use crate::mailspace::PumpAction;
use crate::pump::config::PumpConfig;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::index::{RunRow, StepState};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::requeue::{RequeueRequest, apply_batch, run, would_line};
use crate::pump::state::{Attempt, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The fake clock's start.
const T0: u64 = 1_000_000;

/// A context with default config, home at `dir/home`, and a paused clock.
fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    (dir, ctx, host)
}

/// A requeue request.
fn request(
    uid: &str,
    attempts: Option<u32>,
    fresh: bool,
    refund: bool,
    reason: &str,
) -> RequeueRequest {
    RequeueRequest {
        unit: uid.to_owned(),
        attempts,
        fresh,
        refund,
        reason: reason.to_owned(),
    }
}

/// Apply one request the way the pump's tick would.
fn apply(ctx: &Ctx, st: &mut State, request: RequeueRequest) {
    apply_batch(ctx, st, &[request]);
}

/// Put one unit into `st`.
fn put(st: &mut State, uid: &str, unit: Unit) {
    st.units.insert(uid.to_owned(), unit);
}

/// True when `line` was logged exactly.
fn logged(host: &FakeHost, line: &str) -> bool {
    host.logs().iter().any(|entry| entry == line)
}

#[test]
fn requeue_applies_a_stuck_unit() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "ls-remote",
        0,
        "aaa\trefs/heads/vivi/unit/u1-a1\nbbb\trefs/heads/vivi/unit/u1-a2\n",
    );
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 1,
            task: Some(String::from("task-1")),
            held_on: Some(String::from("u0")),
            since: 1,
            ..Unit::default()
        },
    );
    apply(
        &ctx,
        &mut st,
        request("u1", None, false, false, "operator asked"),
    );

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 2);
    assert_eq!(unit.task, None);
    assert_eq!(unit.held_on, None);
    assert_eq!(unit.since, T0);
    assert!(logged(&host, "requeue u1 by operator: operator asked"));
}

#[test]
fn requeue_never_collides_with_an_existing_branch() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "ls-remote",
        0,
        concat!(
            "aaa\trefs/heads/vivi/unit/u1-a1\n",
            "bbb\trefs/heads/vivi/unit/u1-a2\n",
            "ccc\trefs/heads/vivi/unit/u1-a3\n",
        ),
    );
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 3,
            ..Unit::default()
        },
    );
    apply(
        &ctx,
        &mut st,
        request("u1", None, true, false, "start over"),
    );

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 3, "fresh still starts past every branch");
    assert_eq!(unit.attempts + 1, 4, "the next attempt branch is a4");
    assert!(logged(
        &host,
        "requeue u1 attempts raised to 3: branch vivi/unit/u1-a3 already exists"
    ));
}

#[test]
fn requeue_releases_a_quarantined_unit_solo() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 0, "");
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Quarantined,
            quarantine: Some(String::from("q1")),
            attempts: 2,
            ..Unit::default()
        },
    );
    apply(
        &ctx,
        &mut st,
        request("u1", None, false, false, "retry alone"),
    );

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Delivered);
    assert!(unit.solo);
    assert_eq!(unit.quarantine, None);
    assert_eq!(unit.attempts, 2);
    assert!(logged(&host, "requeue u1 by operator: retry alone"));
}

#[test]
fn requeue_refuses_a_merged_unit() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 0, "");
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Merged,
            attempts: 2,
            commit: Some(String::from("abc123")),
            ..Unit::default()
        },
    );
    apply(&ctx, &mut st, request("u1", None, true, false, "too late"));

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Merged);
    assert_eq!(unit.attempts, 2);
    assert_eq!(unit.commit.as_deref(), Some("abc123"));
    assert!(logged(&host, "requeue u1 refused: status is merged"));
}

#[test]
fn requeue_refunds_transient_history() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 0, "");
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 3,
            transient_retries: 2,
            history: vec![
                Attempt {
                    attempt: 1,
                    why: String::from("error/host-down"),
                    ..Attempt::default()
                },
                Attempt {
                    attempt: 2,
                    why: String::from("fetch-failed"),
                    ..Attempt::default()
                },
                Attempt {
                    attempt: 3,
                    why: String::from("proof-failed"),
                    detail: String::from("2 tests failed (exit 1)"),
                    ..Attempt::default()
                },
            ],
            ..Unit::default()
        },
    );
    apply(
        &ctx,
        &mut st,
        request("u1", None, false, true, "refund the host failures"),
    );

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 1);
    assert_eq!(unit.transient_retries, 0);
    assert!(logged(
        &host,
        "requeue u1 by operator: refund the host failures"
    ));
}

/// Write `units` into the project's `pump.db`.
fn seed_units(project: &Project, units: &[(&str, Unit)]) {
    let mut st = State::default();
    for (uid, unit) in units {
        put(&mut st, uid, unit.clone());
    }
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    store.save(&st).unwrap();
}

/// A stuck unit of `need`.
fn stuck_of(need: &str) -> Unit {
    Unit {
        need: need.to_owned(),
        status: Status::Stuck,
        attempts: 1,
        task: Some(String::from("task-1")),
        since: 1,
        ..Unit::default()
    }
}

/// A requeue command with `units` and nothing else set.
fn command(units: &[&str]) -> PumpRequeueCommand {
    PumpRequeueCommand {
        units: units.iter().map(|uid| (*uid).to_owned()).collect(),
        attempts: None,
        refund_host_failures: false,
        fresh: false,
        reason: None,
        dry_run: false,
        stuck_since: None,
        why: None,
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

#[test]
fn run_requeue_posts_an_action_on_the_needs_open_record() {
    let project = project();
    let need = project.need("fix auth");
    let record = project.mailspace.pump_send(None, &need, None).unwrap();
    seed_units(&project, &[("u1", stuck_of(&need))]);

    run(project.root(), &command(&["u1"])).unwrap();

    let actions = posted(&project);
    assert_eq!(actions.len(), 1, "one action was posted");
    assert_eq!(actions[0].record, record.handle);
    assert_eq!(actions[0].need, need);
    assert_eq!(actions[0].request.unit.as_deref(), Some("u1"));
    assert_eq!(actions[0].request.reason, "operator requeue");
    assert_eq!(actions[0].request.attempts, None);

    // End to end without a running pump: the next tick flips the unit.
    let host = FakeHost::new();
    host.set_now(T0);
    host.reply("ls-remote", 0, "");
    let mut ctx = project.pump_ctx();
    ctx.host = host;
    let mut st = PumpStore::open_read_only(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .unwrap()
        .load()
        .unwrap();
    crate::pump::action::apply_pending(&ctx, &mut st);
    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.task, None);
    assert_eq!(unit.attempts, 1, "no branch exists, so no bump");
}

#[test]
fn run_requeue_is_refused_when_the_needs_have_no_open_record_and_posts_nothing() {
    let project = project();
    let need = project.need("fix auth");
    let other = project.need("fix billing");
    project.mailspace.pump_send(None, &need, None).unwrap();
    seed_units(
        &project,
        &[("u1", stuck_of(&need)), ("u2", stuck_of(&other))],
    );

    let error = run(project.root(), &command(&["u1", "u2"])).unwrap_err();

    let text = error.to_string();
    assert!(text.contains("unit u2 belongs to need"), "{text}");
    assert!(text.contains("no open pump record"), "{text}");
    assert!(
        text.contains(&format!("vivi pump send --need {other}")),
        "{text}"
    );
    assert!(posted(&project).is_empty(), "u1 must not be posted either");
}

#[test]
fn run_requeue_dry_run_posts_nothing() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    seed_units(&project, &[("u1", stuck_of(&need))]);

    let mut dry = command(&["u1"]);
    dry.dry_run = true;
    run(project.root(), &dry).unwrap();
    assert!(posted(&project).is_empty(), "a dry run posts no action");

    // An explicit attempt count is what the would-line reports.
    dry.attempts = Some(3);
    run(project.root(), &dry).unwrap();
    assert!(posted(&project).is_empty());
    assert_eq!(
        would_line("u1", Status::Stuck, 1, 3),
        "would requeue u1: status stuck, attempts 1 -> 3, next branch vivi/unit/u1-a4"
    );
}

#[test]
fn run_requeue_rejects_unknown_uid_and_conflicting_flags() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    seed_units(&project, &[("u1", stuck_of(&need))]);

    let err = run(project.root(), &command(&["nope"])).unwrap_err();
    assert!(err.to_string().contains("unknown unit: nope"), "{err}");

    let mut conflicting = command(&["u1"]);
    conflicting.attempts = Some(2);
    conflicting.fresh = true;
    let err = run(project.root(), &conflicting).unwrap_err();
    assert!(err.to_string().contains("use only one of"), "{err}");

    let mut bulk = command(&[]);
    bulk.stuck_since = Some(String::from("1h"));
    let err = run(project.root(), &bulk).unwrap_err();
    assert!(err.to_string().contains("requires --why"), "{err}");

    let err = run(project.root(), &command(&[])).unwrap_err();
    assert!(err.to_string().contains("at least one unit"), "{err}");

    assert!(
        posted(&project).is_empty(),
        "a refused command posts nothing"
    );
}

/// Epoch seconds right now, for the bulk form's wall-clock window.
fn wall_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A stuck unit of `need` whose last attempt landed `age` seconds ago.
fn stuck_aged(need: &str, age: u64) -> Unit {
    let at = wall_now() - age;
    Unit {
        need: need.to_owned(),
        status: Status::Stuck,
        attempts: 1,
        history: vec![Attempt {
            attempt: 1,
            why: String::from("proof-failed"),
            at,
            ..Attempt::default()
        }],
        since: at,
        ..Unit::default()
    }
}

#[test]
fn run_requeue_bulk_selects_recent_stuck_units() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    let merged = Unit {
        need: need.clone(),
        status: Status::Merged,
        since: wall_now(),
        ..Unit::default()
    };
    seed_units(
        &project,
        &[
            ("u-stale", stuck_aged(&need, 7200)),
            ("u-fresh", stuck_aged(&need, 60)),
            ("u-merged", merged),
        ],
    );

    let mut bulk = command(&[]);
    bulk.stuck_since = Some(String::from("5m"));
    bulk.why = Some(String::from("host-down"));
    run(project.root(), &bulk).unwrap();

    let actions = posted(&project);
    assert_eq!(actions.len(), 1, "only the fresh stuck unit is selected");
    let request = &actions[0].request;
    assert_eq!(request.unit.as_deref(), Some("u-fresh"));
    assert_eq!(request.reason, "bulk host-down");
    assert_eq!(
        request.attempts, None,
        "the bulk form keeps the attempt count"
    );
    assert!(!request.fresh && !request.refund, "default policy");

    // An empty window is a successful no-op.
    let mut narrow = command(&[]);
    narrow.stuck_since = Some(String::from("30s"));
    narrow.why = Some(String::from("host-down"));
    run(project.root(), &narrow).unwrap();
    assert_eq!(posted(&project).len(), 1, "nothing recent is selected");
}

#[test]
fn run_requeue_bulk_skips_units_whose_need_has_no_open_record() {
    let project = project();
    let open = project.need("fix auth");
    let closed = project.need("fix billing");
    project.mailspace.pump_send(None, &open, None).unwrap();
    seed_units(
        &project,
        &[
            ("u-open", stuck_aged(&open, 60)),
            ("u-closed", stuck_aged(&closed, 60)),
        ],
    );

    let mut bulk = command(&[]);
    bulk.stuck_since = Some(String::from("5m"));
    bulk.why = Some(String::from("host-down"));
    run(project.root(), &bulk).unwrap();

    let actions = posted(&project);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].request.unit.as_deref(), Some("u-open"));
}

#[test]
fn run_requeue_bulk_requires_why_and_forbids_uids() {
    let project = project();
    let need = project.need("fix auth");
    project.mailspace.pump_send(None, &need, None).unwrap();
    seed_units(&project, &[("u1", stuck_of(&need))]);

    let mut no_why = command(&[]);
    no_why.stuck_since = Some(String::from("30s"));
    let err = run(project.root(), &no_why).unwrap_err();
    assert!(err.to_string().contains("requires --why"), "{err}");

    let mut with_uids = command(&["u1"]);
    with_uids.stuck_since = Some(String::from("30s"));
    with_uids.why = Some(String::from("host-down"));
    let err = run(project.root(), &with_uids).unwrap_err();
    assert!(err.to_string().contains("no unit uids"), "{err}");

    assert!(
        posted(&project).is_empty(),
        "a refused bulk form posts nothing"
    );
}

#[test]
fn run_requeue_without_a_pump_database_is_refused() {
    let project = project();
    let err = run(project.root(), &command(&["u1"])).unwrap_err();
    assert!(err.to_string().contains("no pump.db at"), "{err}");
}

/// Seed a `live` flow run of `need` holding one step with `task`.
fn seed_flow_step(project: &Project, need: &str, task: &str) {
    let mut st = State::default();
    st.units.insert(String::from("u1"), stuck_of(need));
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    store.save(&st).unwrap();
    let run = RunRow {
        need: need.to_owned(),
        run: 1,
        flow: String::from("build"),
        flow_version: 2,
        kind: String::from("build"),
        entry: String::from("plan"),
        state: String::from("live"),
    };
    store.insert_run(&run).unwrap();
    store
        .insert_run_step(&run, &run.step("plan", task, StepState::Pending))
        .unwrap();
}

#[test]
fn run_requeue_posts_a_requeue_action_for_a_flow_step_task() {
    let project = project();
    let need = project.need("fix auth");
    let record = project.mailspace.pump_send(None, &need, None).unwrap();
    seed_flow_step(&project, &need, "step-task-1");

    let mut dry = command(&["step-task-1"]);
    dry.dry_run = true;
    run(project.root(), &dry).unwrap();
    assert!(posted(&project).is_empty(), "a dry run posts nothing");

    run(project.root(), &command(&["step-task-1"])).unwrap();

    let actions = posted(&project);
    assert_eq!(actions.len(), 1, "one action was posted");
    assert_eq!(actions[0].record, record.handle);
    assert_eq!(actions[0].need, need);
    assert_eq!(actions[0].request.unit.as_deref(), Some("step-task-1"));
    assert_eq!(actions[0].request.reason, "operator requeue");
}

#[test]
fn run_requeue_of_a_flow_step_needs_an_open_record_and_a_known_task() {
    let project = project();
    let need = project.need("fix auth");
    seed_flow_step(&project, &need, "step-task-1");

    let text = run(project.root(), &command(&["step-task-1"]))
        .unwrap_err()
        .to_string();
    assert!(text.contains("no open pump record"), "{text}");
    assert!(posted(&project).is_empty());

    let text = run(project.root(), &command(&["neither"]))
        .unwrap_err()
        .to_string();
    assert!(text.contains("unknown unit: neither"), "{text}");
}
