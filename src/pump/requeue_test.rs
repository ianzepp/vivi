//! Tests for [`crate::pump::requeue`]: operator requests applied to state,
//! with the attempt counter kept clear of branches that already exist.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::cli::PumpRequeueCommand;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::Merger;
use crate::pump::requeue::{apply_pending, run, would_line};
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

/// Queue one requeue request in the pump store.
fn request(ctx: &Ctx, uid: &str, attempts: Option<i64>, fresh: bool, refund: bool, reason: &str) {
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store
        .add_requeue_request(uid, attempts, fresh, refund, reason, T0)
        .unwrap();
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
        "aaa\trefs/heads/unit/u1-a1\nbbb\trefs/heads/unit/u1-a2\n",
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
    request(&ctx, "u1", None, false, false, "operator asked");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 2);
    assert_eq!(unit.task, None);
    assert_eq!(unit.held_on, None);
    assert_eq!(unit.since, T0);
    assert!(logged(&host, "requeue u1 by operator: operator asked"));

    // The request is consumed: a second tick changes nothing.
    let before = host.logs().len();
    apply_pending(&ctx, &mut st);
    assert_eq!(host.logs().len(), before);
    assert_eq!(st.units["u1"].attempts, 2);
}

#[test]
fn requeue_never_collides_with_an_existing_branch() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "ls-remote",
        0,
        concat!(
            "aaa\trefs/heads/unit/u1-a1\n",
            "bbb\trefs/heads/unit/u1-a2\n",
            "ccc\trefs/heads/unit/u1-a3\n",
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
    request(&ctx, "u1", None, true, false, "start over");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 3, "fresh still starts past every branch");
    assert_eq!(unit.attempts + 1, 4, "the next attempt branch is a4");
    assert!(logged(
        &host,
        "requeue u1 attempts raised to 3: branch unit/u1-a3 already exists"
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
    request(&ctx, "u1", None, false, false, "retry alone");

    apply_pending(&ctx, &mut st);

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
    request(&ctx, "u1", None, true, false, "too late");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Merged);
    assert_eq!(unit.attempts, 2);
    assert_eq!(unit.commit.as_deref(), Some("abc123"));
    assert!(logged(&host, "requeue u1 refused: status is merged"));

    // Refused or not, the request is consumed.
    request(&ctx, "u1", None, false, false, "again");
    let before = host.logs().len();
    apply_pending(&ctx, &mut st);
    assert_eq!(host.logs().len(), before + 1);
    let store = PumpStore::open_read_only(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .unwrap();
    assert!(store.pending_requeue_requests().unwrap().is_empty());
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
    request(&ctx, "u1", None, false, true, "refund the host failures");

    apply_pending(&ctx, &mut st);

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.attempts, 1);
    assert_eq!(unit.transient_retries, 0);
    assert!(logged(
        &host,
        "requeue u1 by operator: refund the host failures"
    ));
}

#[test]
fn a_tick_applies_a_pending_requeue_request() {
    let (_dir, ctx, host) = setup();
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 1,
            task: Some(String::from("task-1")),
            since: 1,
            ..Unit::default()
        },
    );
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();
    store
        .add_requeue_request("u1", None, false, false, "operator asked", T0)
        .unwrap();
    drop(store);

    let merger = Merger::start(&ctx);
    crate::pump::run::tick(&ctx, &mut st, &merger).unwrap();

    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending, "the tick did not requeue u1");
    assert_eq!(unit.task, None);
    assert_eq!(unit.attempts, 1, "no branch exists, so no bump");
    assert!(logged(&host, "requeue u1 by operator: operator asked"));
    let store = PumpStore::open_read_only(&ctx.home.join(DB_FILENAME))
        .unwrap()
        .unwrap();
    assert!(
        store.pending_requeue_requests().unwrap().is_empty(),
        "the request was not consumed"
    );
}

/// A mailspace project directory carrying a `[pump]` table only, so the
/// default pump home (`<project>/.vivi/pump`) applies.
fn mailspace_project() -> TempDir {
    let dir = tempdir().unwrap();
    let vivi = dir.path().join(".vivi");
    std::fs::create_dir_all(&vivi).unwrap();
    std::fs::write(
        vivi.join("mailspace.toml"),
        "name = \"requeue\"\n\n[pump]\nbranch = \"next\"\n",
    )
    .unwrap();
    dir
}

/// The pump home of a project from [`mailspace_project`].
fn project_home(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join(".vivi").join("pump")
}

/// Write one stuck unit into the project's `pump.db`.
fn seed_stuck_unit(dir: &TempDir) {
    let home = project_home(dir);
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 1,
            task: Some(String::from("task-1")),
            since: 1,
            ..Unit::default()
        },
    );
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    store.save(&st).unwrap();
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

/// A context over the project's pump home with a [`FakeHost`], for applying
/// the queued request the way the pump's tick would.
fn apply_ctx(dir: &TempDir) -> (Ctx, Arc<FakeHost>) {
    let home = project_home(dir);
    let host = FakeHost::new();
    host.set_now(T0);
    let ctx = Ctx::with_parts(
        PumpConfig::default(),
        host.clone(),
        home.clone(),
        String::from("vivi"),
        String::new(),
        home.clone(),
        home.join("repo"),
    );
    (ctx, host)
}

/// The pending request rows of the project's `pump.db`.
fn pending(dir: &TempDir) -> Vec<crate::pump::store::RequeueRequest> {
    let path = project_home(dir).join(DB_FILENAME);
    PumpStore::open_read_only(&path)
        .unwrap()
        .unwrap()
        .pending_requeue_requests()
        .unwrap()
}

#[test]
fn run_requeue_writes_a_request_row() {
    let dir = mailspace_project();
    seed_stuck_unit(&dir);

    run(dir.path(), &command(&["u1"])).unwrap();

    let rows = pending(&dir);
    assert_eq!(rows.len(), 1, "one request row was written");
    assert_eq!(rows[0].unit, "u1");
    assert_eq!(rows[0].reason, "operator requeue");
    assert_eq!(rows[0].attempts, None);

    // End to end without a running pump: the next apply flips the unit.
    let (ctx, _host) = apply_ctx(&dir);
    let mut st = State::default();
    put(
        &mut st,
        "u1",
        Unit {
            status: Status::Stuck,
            attempts: 1,
            task: Some(String::from("task-1")),
            since: 1,
            ..Unit::default()
        },
    );
    apply_pending(&ctx, &mut st);
    let unit = &st.units["u1"];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.task, None);
    assert_eq!(unit.attempts, 1, "no branch exists, so no bump");
    assert!(pending(&dir).is_empty(), "the request was consumed");
}

#[test]
fn run_requeue_dry_run_writes_no_rows() {
    let dir = mailspace_project();
    seed_stuck_unit(&dir);

    let mut dry = command(&["u1"]);
    dry.dry_run = true;
    run(dir.path(), &dry).unwrap();
    assert!(pending(&dir).is_empty(), "a dry run writes no request");

    // An explicit attempt count is what the would-line reports.
    dry.attempts = Some(3);
    run(dir.path(), &dry).unwrap();
    assert!(pending(&dir).is_empty());
    assert_eq!(
        would_line("u1", Status::Stuck, 1, 3),
        "would requeue u1: status stuck, attempts 1 -> 3, next branch unit/u1-a4"
    );
}

#[test]
fn run_requeue_rejects_unknown_uid_and_conflicting_flags() {
    let dir = mailspace_project();
    seed_stuck_unit(&dir);

    let err = run(dir.path(), &command(&["nope"])).unwrap_err();
    assert!(err.to_string().contains("unknown unit: nope"), "{err}");

    let mut conflicting = command(&["u1"]);
    conflicting.attempts = Some(2);
    conflicting.fresh = true;
    let err = run(dir.path(), &conflicting).unwrap_err();
    assert!(err.to_string().contains("use only one of"), "{err}");

    let mut bulk = command(&[]);
    bulk.stuck_since = Some(String::from("1h"));
    let err = run(dir.path(), &bulk).unwrap_err();
    assert!(err.to_string().contains("requires --why"), "{err}");

    let err = run(dir.path(), &command(&[])).unwrap_err();
    assert!(err.to_string().contains("at least one unit"), "{err}");

    assert!(pending(&dir).is_empty(), "a refused command writes nothing");
}

/// Epoch seconds right now, for the bulk form's wall-clock window.
fn wall_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Write `units` into the project's `pump.db`.
fn seed_units(dir: &TempDir, units: &[(&str, Unit)]) {
    let home = project_home(dir);
    let mut st = State::default();
    for (uid, unit) in units {
        put(&mut st, uid, unit.clone());
    }
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    store.save(&st).unwrap();
}

/// A stuck unit whose last attempt landed `age` seconds ago.
fn stuck_aged(age: u64) -> Unit {
    let at = wall_now() - age;
    Unit {
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
    let dir = mailspace_project();
    let merged = Unit {
        status: Status::Merged,
        since: wall_now(),
        ..Unit::default()
    };
    seed_units(
        &dir,
        &[
            ("u-stale", stuck_aged(7200)),
            ("u-fresh", stuck_aged(60)),
            ("u-merged", merged),
        ],
    );

    let mut bulk = command(&[]);
    bulk.stuck_since = Some(String::from("5m"));
    bulk.why = Some(String::from("host-down"));
    run(dir.path(), &bulk).unwrap();

    let rows = pending(&dir);
    assert_eq!(rows.len(), 1, "only the fresh stuck unit is selected");
    assert_eq!(rows[0].unit, "u-fresh");
    assert_eq!(rows[0].reason, "bulk host-down");
    assert_eq!(
        rows[0].attempts, None,
        "the bulk form keeps the attempt count"
    );
    assert!(!rows[0].fresh && !rows[0].refund, "default policy");

    // An empty window is a successful no-op once the requests are drained.
    let mut store = PumpStore::open(&project_home(&dir).join(DB_FILENAME)).unwrap();
    let pending_ids: Vec<i64> = store
        .pending_requeue_requests()
        .unwrap()
        .iter()
        .map(|row| row.request_id)
        .collect();
    for request_id in pending_ids {
        store.mark_requeue_applied(request_id, T0).unwrap();
    }
    drop(store);
    let mut narrow = command(&[]);
    narrow.stuck_since = Some(String::from("30s"));
    narrow.why = Some(String::from("host-down"));
    run(dir.path(), &narrow).unwrap();
    assert!(pending(&dir).is_empty(), "nothing recent is selected");
}

#[test]
fn run_requeue_bulk_requires_why_and_forbids_uids() {
    let dir = mailspace_project();
    seed_stuck_unit(&dir);

    let mut no_why = command(&[]);
    no_why.stuck_since = Some(String::from("30s"));
    let err = run(dir.path(), &no_why).unwrap_err();
    assert!(err.to_string().contains("requires --why"), "{err}");

    let mut with_uids = command(&["u1"]);
    with_uids.stuck_since = Some(String::from("30s"));
    with_uids.why = Some(String::from("host-down"));
    let err = run(dir.path(), &with_uids).unwrap_err();
    assert!(err.to_string().contains("no unit uids"), "{err}");

    assert!(
        pending(&dir).is_empty(),
        "a refused bulk form writes nothing"
    );
}

#[test]
fn run_requeue_without_a_pump_database_is_refused() {
    let dir = mailspace_project();
    let err = run(dir.path(), &command(&["u1"])).unwrap_err();
    assert!(err.to_string().contains("no pump.db at"), "{err}");
}
