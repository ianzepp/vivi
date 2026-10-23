//! Tests for [`crate::pump::run`]: the status file, the tick order, the
//! flags, the lock, and the run/status entry points.

use std::fs::{self, File};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use crate::mailspace::Mailspace;
use crate::pump::boardpass::BoardPass;
use crate::pump::config::{BoardConfig, DispatchConfig, Kind, Lane, PumpConfig, PumpTarget};
use crate::pump::host::{CmdOutput, Ctx, FakeHost, Host, test_ctx};
use crate::pump::jobs::{JobSpec, JobStatus, PoolJob};
use crate::pump::run::{
    actor_string, ensure_repo, refresh_config, run, run_with, status, tick as run_tick,
    write_status,
};
use crate::pump::state::{Attempt, NeedRecord, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

/// The fake clock's start (1970-01-12T13:46:40Z).
const T0: u64 = 1_000_000;

fn tick(ctx: &Ctx, st: &mut State) {
    let (mut pass, requests, results) = BoardPass::detached();
    run_tick(ctx, st, &mut pass).unwrap();
    pass.serve(ctx, st, &requests, &results);
}

/// A mailspace whose `[pump]` table passes `validate` with a pool-backed
/// planner role.
const PUMP_TOML: &str = concat!(
    "name = \"pump\"\n\n",
    "[[identities]]\nname = \"pump-planner\"\nkind = \"planner\"\npool = \"here\"\nmodel = \"P2\"\n\n",
    "[[identities]]\nname = \"merge\"\nkind = \"merge\"\npool = \"here\"\nmodel = \"P2\"\n\n",
    "[[identities]]\nname = \"auditor\"\nkind = \"auditor\"\npool = \"here\"\nmodel = \"P2\"\n\n",
    "[pools.here]\nprovider = \"local\"\nslots = 1\n\n",
    "[pools.here.harness]\nname = \"fake\"\nrun = \"fake --model {model} @{brief}\"\nmodels = [\"m/ok\"]\n\n",
    "[pools.here.bands]\nP2 = [{ model = \"m/ok\", reasoning = \"high\" }]\n\n",
    "[pump]\ntick = 1\n",
    "committer_name = \"Pump\"\ncommitter_email = \"pump@example.com\"\n\n",
    "[pump.dispatch]\nlanes = [{ model = \"m\", cap = 2 }]\n\n",
);

/// The roster roles the default `[pump]` identities name, so a project
/// carrying them starts cleanly.
const PUMP_TOML_IDENTITIES: &str = concat!(
    "[[identities]]\nname = \"operator\"\n\n",
    "[[identities]]\nname = \"hand\"\nkind = \"hand\"\n\n",
    "[[identities]]\nname = \"mind\"\n",
);

/// A context with the default config, home at `dir/home`.
fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

/// A unit in `status`, holding `task`, created at the fake clock's start.
fn unit(status: Status, task: Option<&str>) -> Unit {
    Unit {
        need: String::from("n1"),
        title: String::from("a unit"),
        status,
        task: task.map(str::to_string),
        since: T0,
        ..Unit::default()
    }
}

/// Add `uid` to the state's order and units.
fn put(st: &mut State, uid: &str, unit: Unit) {
    st.order.push(uid.to_owned());
    st.units.insert(uid.to_owned(), unit);
}

/// A temp project root carrying `.vivi/mailspace.toml` with `toml`.
fn mailspace_project(toml: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(dir.path().join(".vivi").join("mailspace.toml"), toml).unwrap();
    dir
}

#[test]
fn config_changes_are_applied_before_the_next_tick() {
    let initial = format!("{PUMP_TOML}{PUMP_TOML_IDENTITIES}");
    let dir = mailspace_project(&initial);
    let mailspace = Mailspace::discover(Some(dir.path())).unwrap();
    let cfg = mailspace.config.pump.clone().unwrap();
    let host = FakeHost::new();
    let mut ctx = Ctx::new(cfg, host, dir.path().to_path_buf(), String::from("vivi"))
        .with_routing(crate::routing::from_mailspace(&mailspace.config))
        .with_actor(actor_string(&mailspace))
        .with_mailspace(mailspace.mailspace_id().to_string())
        .with_pool_sources(
            mailspace
                .config
                .pump
                .as_ref()
                .unwrap()
                .pool_sources(&mailspace.config, dir.path()),
        );
    let mut fingerprint = None;
    let mut last_error = None;

    assert!(!refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert_eq!(ctx.cfg.dispatch.lanes[0].cap, 2);

    let changed = initial.replace("cap = 2", "cap = 7");
    fs::write(dir.path().join(".vivi/mailspace.toml"), changed).unwrap();
    assert!(refresh_config(&mut ctx, &mut fingerprint, &mut last_error));
    assert_eq!(ctx.cfg.dispatch.lanes[0].cap, 7);
    assert!(last_error.is_none());
}

/// The first index of a command line containing `fragment`.
fn first_at(commands: &[String], fragment: &str) -> usize {
    commands
        .iter()
        .position(|command| command.contains(fragment))
        .unwrap_or(usize::MAX)
}

#[test]
fn write_status_writes_counts_then_the_delivered_line() {
    let (dir, ctx, _host) = setup();
    let mut st = State::default();
    put(&mut st, "a-1", unit(Status::Pending, None));
    put(&mut st, "a-2", unit(Status::Pending, None));
    put(&mut st, "a-3", unit(Status::Merged, Some("9d98468e")));
    for handle in ["n1", "n2"] {
        st.needs.insert(
            handle.to_owned(),
            NeedRecord {
                verdict: String::from("closed"),
                ..NeedRecord::default()
            },
        );
    }
    st.needs.insert(
        String::from("n3"),
        NeedRecord {
            verdict: String::from("units"),
            ..NeedRecord::default()
        },
    );
    st.merges = 3;
    st.closed_needs = vec![String::from("n1"), String::from("n2")];
    write_status(&ctx, &st);
    let text = fs::read_to_string(dir.path().join("home/status.txt")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "{text}");
    assert_eq!(lines[0], "1970-01-12T13:46:40Z");
    assert_eq!(lines[1], "units merged=1 pending=2");
    assert_eq!(lines[2], "plans closed=2 units=1");
    assert_eq!(lines[3], "merges 3  needs-ready 2");
    assert_eq!(lines[4], "delivered waiting 0, oldest none");
}

#[test]
fn write_status_lists_top_blockers_after_the_stall_lines() {
    let (dir, ctx, host) = setup();
    let mut st = State::default();
    st.needs.insert(
        String::from("r"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("r-1")],
            ..NeedRecord::default()
        },
    );
    st.units.insert(
        String::from("r-1"),
        Unit {
            need: String::from("r"),
            status: Status::Stuck,
            since: T0 - 30 * 60,
            ..Unit::default()
        },
    );
    let mut pairs: Vec<serde_json::Value> = Vec::new();
    for index in 1..=12 {
        let need = format!("m{index}");
        st.needs.insert(
            need.clone(),
            NeedRecord {
                verdict: String::from("units"),
                ..NeedRecord::default()
            },
        );
        st.units.insert(
            format!("{need}-1"),
            Unit {
                need: need.clone(),
                status: Status::Pending,
                since: T0 - 600,
                ..Unit::default()
            },
        );
        pairs.push(serde_json::json!({"source_id": need, "blocked_by": ["r"]}));
    }
    host.reply(
        "graph blocked",
        0,
        &serde_json::Value::Array(pairs).to_string(),
    );
    host.set_now(T0);
    write_status(&ctx, &st);
    let text = fs::read_to_string(dir.path().join("home/status.txt")).unwrap();
    assert!(
        text.contains("blocker r stuck age 30m held 13"),
        "no blocker line in {text}"
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("blocker "))
            .count(),
        1
    );
    let blocker = lines
        .iter()
        .position(|line| line.starts_with("blocker "))
        .unwrap();
    let stall = lines
        .iter()
        .position(|line| line.starts_with("delivered waiting "))
        .unwrap();
    assert!(blocker > stall, "blocker follows stall status: {text}");
}

#[test]
fn write_status_lists_durable_planner_jobs_with_pool_and_age() {
    let (dir, ctx, _host) = setup();
    let mut st = State::default();
    let now = ctx.now();
    st.jobs.insert(
        "plan1".into(),
        planner_job("plan1", JobStatus::Dispatched, now - 310, Some(now - 310)),
    );
    st.jobs.insert(
        "plan2".into(),
        planner_job("plan2", JobStatus::Pending, now, None),
    );
    st.board.planner_fails.insert(String::from("n7"), 2);
    write_status(&ctx, &st);
    let text = fs::read_to_string(dir.path().join("home/status.txt")).unwrap();
    assert!(
        text.contains("planner-job plan1 in-flight pool=here task=task-plan1 age=310s"),
        "no n1 planner line in {text}"
    );
    assert!(
        text.contains("planner-job plan2 queued pool=here task=pending age=0s"),
        "no n2 planner line in {text}"
    );
    assert!(
        text.contains("planner-failures n7=2"),
        "no failure line in {text}"
    );
    assert!(
        !text.contains("planner-parked"),
        "the parked line is gone in {text}"
    );
}

fn planner_job(id: &str, status: JobStatus, queued: u64, dispatched: Option<u64>) -> PoolJob {
    let active = status == JobStatus::Dispatched;
    let mut job = PoolJob::from(JobSpec::new(
        id,
        "planner:board",
        "planner",
        format!("planner {id}"),
        String::new(),
    ));
    job.queued_at = queued;
    job.pool_candidates = vec!["here".into()];
    job.read_only = true;
    job.task = active.then(|| format!("task-{id}"));
    job.pool = active.then(|| String::from("here"));
    job.dispatched_at = dispatched;
    job.status = status;
    job
}

#[test]
fn write_status_adds_no_planner_lines_when_idle() {
    let (dir, ctx, _host) = setup();
    let st = State::default();
    write_status(&ctx, &st);
    let text = fs::read_to_string(dir.path().join("home/status.txt")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.iter().any(|line| line.starts_with("merges ")));
    assert!(
        !text.contains("planner"),
        "idle board wrote planner lines in {text}"
    );
}

#[test]
fn a_tick_runs_the_steps_in_pump_py_order() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 running \"agent pid 7\"",
    );
    let mut st = State::default();
    let mut job = planner_job("j-1", JobStatus::Dispatched, ctx.now(), Some(ctx.now()));
    job.task = Some(String::from("1a2b3c4d"));
    st.jobs.insert(String::from("j-1"), job);
    tick(&ctx, &mut st);
    let commands = host.commands();
    let sync = first_at(&commands, "sync pull");
    let reconcile = first_at(&commands, "pool reconcile --porcelain");
    assert!(sync < usize::MAX, "no sync pull in {commands:?}");
    assert!(reconcile < usize::MAX, "no pool reconcile in {commands:?}");
    assert!(reconcile < sync);
}

/// A tick logs how long its collect phase took, so a slow
/// main loop shows up in `<home>/log.txt`.
#[test]
fn a_tick_logs_the_collect_duration() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 running \"agent pid 7\"",
    );
    let mut st = State::default();
    put(&mut st, "a-1", unit(Status::Inflight, Some("1a2b3c4d")));
    tick(&ctx, &mut st);
    assert!(
        host.logs().iter().any(|l| l.starts_with("collect took ")),
        "no collect duration in {:?}",
        host.logs()
    );
}

/// The `graph blocked --kind need --json` answer: twenty needs waiting on
/// the stuck root `r`.
fn twenty_dependents_on_r() -> String {
    let rows: Vec<serde_json::Value> = (1..=20)
        .map(|index| {
            serde_json::json!({
                "source_id": format!("m{index}"),
                "kind": "need",
                "label": format!("M{index}"),
                "blocked_by": ["r"],
            })
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

/// A state with `r-1` stuck at its attempt cap after a timeout and twenty
/// pending units of the needs that wait on `r`.
fn stuck_root_with_twenty_dependents() -> State {
    let mut st = State::default();
    st.needs.insert(
        String::from("r"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("r-1")],
            ..NeedRecord::default()
        },
    );
    put(
        &mut st,
        "r-1",
        Unit {
            need: String::from("r"),
            status: Status::Stuck,
            attempts: 3,
            since: T0 - 3600,
            history: vec![Attempt {
                attempt: 3,
                why: String::from("timeout"),
                detail: String::from("killed by budget"),
                at: T0 - 3600,
                ..Attempt::default()
            }],
            ..Unit::default()
        },
    );
    for index in 1..=20 {
        let need = format!("m{index}");
        st.needs.insert(
            need.clone(),
            NeedRecord {
                verdict: String::from("units"),
                units: vec![format!("{need}-1")],
                ..NeedRecord::default()
            },
        );
        put(
            &mut st,
            &format!("{need}-1"),
            Unit {
                need,
                status: Status::Pending,
                write_scope: vec![format!("src/m{index}/x.rs")],
                since: T0,
                ..Unit::default()
            },
        );
    }
    st
}

#[test]
fn a_stuck_root_with_twenty_dependents_alarms_bumps_and_requeues_in_one_tick() {
    let (dir, ctx, host) = setup();
    host.set_now(T0);
    host.reply("graph blocked", 0, &twenty_dependents_on_r());
    let mut st = stuck_root_with_twenty_dependents();
    tick(&ctx, &mut st);

    let exceptions = fs::read_to_string(dir.path().join("home/exceptions.txt")).unwrap();
    assert!(
        exceptions.contains("blocking-stuck r-1"),
        "no blocking-stuck alarm in {exceptions}"
    );
    assert!(
        exceptions.contains("holds 21 units"),
        "the alarm does not count r-1 plus its dependents: {exceptions}"
    );

    let root = &st.units["r-1"];
    assert_eq!(
        root.status,
        Status::Pending,
        "the stuck root was not requeued"
    );
    assert!(root.requeued, "the requeue was not recorded on r-1");

    let levels = crate::pump::priority::list(&ctx.home).unwrap();
    assert!(levels.contains(&(String::from("r"), 1)), "{levels:?}");

    let status = fs::read_to_string(dir.path().join("home/status.txt")).unwrap();
    let line = status
        .lines()
        .find(|line| line.starts_with("blocker r "))
        .unwrap_or_default();
    assert!(
        line.contains("held 21"),
        "no 21-unit blocker line in {status}"
    );
}

#[test]
fn a_paused_tick_dispatches_nothing_and_still_writes_status() {
    let (dir, ctx, host) = setup();
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.initialize_control("paused").unwrap();
    let mut st = State::default();
    tick(&ctx, &mut st);
    let commands = host.commands();
    assert!(!commands.iter().any(|c| c.contains("pool status")));
    assert!(!commands.iter().any(|c| c.contains("task send")));
    let text = fs::read_to_string(dir.path().join("home/status.txt")).unwrap();
    assert!(text.starts_with("1970-01-12T13:46:40Z\n"), "{text}");
    assert!(
        text.lines().any(|line| line.starts_with("units ")),
        "{text}"
    );
    assert!(
        text.lines().any(|line| line.starts_with("merges ")),
        "{text}"
    );
}

#[test]
fn a_tick_stops_when_its_state_cannot_be_saved() {
    let (_dir, ctx, host) = setup();
    let path = ctx.home.join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(conn);

    let (mut pass, _requests, _results) = BoardPass::detached();
    let err = run_tick(&ctx, &mut State::default(), &mut pass).unwrap_err();

    assert!(err.to_string().contains("state save failed"), "{err}");
    assert!(
        !host
            .commands()
            .iter()
            .any(|command| command.contains("pool status")),
        "dispatch must stop after persistence fails: {:?}",
        host.commands()
    );
}

#[test]
fn a_tick_writes_status_before_its_first_save() {
    let (_dir, ctx, _host) = setup();
    let path = ctx.home.join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(conn);
    let (mut pass, _requests, _results) = BoardPass::detached();

    let err = run_tick(&ctx, &mut State::default(), &mut pass).unwrap_err();

    assert!(err.to_string().contains("state save failed"), "{err}");
    let text = fs::read_to_string(ctx.home.join("status.txt")).unwrap();
    assert!(
        text.starts_with("1970-01-12T13:46:40Z\n"),
        "status was not written before the first save: {text}"
    );
}

#[test]
fn a_second_instance_is_refused_by_the_lock() {
    let dir = mailspace_project(&format!("{PUMP_TOML}{PUMP_TOML_IDENTITIES}"));
    let home = dir.path().join(".vivi").join("pump");
    fs::create_dir_all(&home).unwrap();
    let first = File::create(home.join("lock")).unwrap();
    first.try_lock().unwrap();
    let err = run(dir.path(), true).unwrap_err();
    assert!(err.to_string().contains("another vivi pump is running"));
}

#[test]
fn a_missing_default_roster_role_holds_the_pump_and_names_the_fix_command() {
    // `to` is the default `hand`: a role a fresh project does not have yet.
    let missing_hand =
        PUMP_TOML_IDENTITIES.replace("[[identities]]\nname = \"hand\"\nkind = \"hand\"\n\n", "");
    let dir = mailspace_project(&format!("{PUMP_TOML}{missing_hand}"));
    run(dir.path(), true).expect("a missing role is not ready, not an error");
    let status = fs::read_to_string(dir.path().join(".vivi/pump/status.txt")).unwrap();
    assert!(status.contains("not ready:"), "{status}");
    assert!(status.contains("[pump] to"), "{status}");
    assert!(status.contains("vivi role add hand"), "{status}");
}

#[test]
fn a_named_roster_role_that_does_not_exist_refuses_to_start() {
    let named = PUMP_TOML.replace("[pump]\n", "[pump]\nto = \"hnad\"\n");
    assert_ne!(named, PUMP_TOML, "the fixture must name `to`");
    let dir = mailspace_project(&format!("{named}{PUMP_TOML_IDENTITIES}"));
    let err = run(dir.path(), true).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("[pump] to"), "{text}");
    assert!(text.contains("unknown local role 'hnad'"), "{text}");
    assert!(text.contains("vivi role add hnad"), "{text}");
}

#[test]
fn once_runs_exactly_one_tick() {
    let (dir, ctx, host) = setup();
    run_with(&ctx, true).unwrap();
    // One tick requests one board pass, which pulls once. A second tick
    // would pull again.
    let pulls = host
        .commands()
        .iter()
        .filter(|c| c.contains("sync pull"))
        .count();
    assert_eq!(pulls, 1);
    assert!(dir.path().join("home").join(DB_FILENAME).is_file());
    let cycles = crate::pump::cycle::read(&dir.path().join("home")).unwrap();
    assert_eq!(cycles.len(), 1);
    assert!(cycles[0].is_finished());
    assert_eq!(cycles[0].outcome.as_deref(), Some("ok"));
    assert!(cycles[0].duration_secs.is_some());
}

#[test]
fn stopped_control_state_prevents_the_run_loop_from_ticking() {
    let (dir, ctx, host) = setup();
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.initialize_control("stopped").unwrap();
    run_with(&ctx, false).unwrap();
    // A stopped pump does not run a new tick.
    let statuses = host
        .commands()
        .iter()
        .filter(|c| c.contains("pool status --porcelain"))
        .count();
    assert_eq!(statuses, 0);
    assert!(dir.path().join("home").join(DB_FILENAME).is_file());
}

#[test]
fn status_reads_the_status_file_or_says_no_status_yet() {
    let with_file = mailspace_project("name = \"s\"\n\n[pump]\nhome = \"pump-home\"\n");
    let home = with_file.path().join("pump-home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("status.txt"), "1970-01-12T13:46:40Z\nunits \n").unwrap();
    let shown = status(with_file.path()).unwrap();
    assert!(
        shown.starts_with(
            "1970-01-12T13:46:40Z\nunits \ncontrol  running\nwriter   not running\nage "
        ),
        "{shown}"
    );
    let without_file = mailspace_project("name = \"s\"\n\n[pump]\nhome = \"pump-home\"\n");
    assert_eq!(
        status(without_file.path()).unwrap(),
        "no status yet\ncontrol  running\nwriter   not running\nage      never\n"
    );
    let no_mailspace = tempdir().unwrap();
    assert_eq!(
        status(no_mailspace.path()).unwrap(),
        "no status yet\ncontrol  running\nwriter   not running\nage      never\n"
    );
}

#[test]
fn status_names_a_stopped_pump_and_a_running_writer() {
    let dir = mailspace_project("name = \"s\"\n\n[pump]\nhome = \"pump-home\"\n");
    let home = dir.path().join("pump-home");
    fs::create_dir_all(&home).unwrap();
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.initialize_control("stopped").unwrap();
    // The store holds the migration lock until it is dropped.
    drop(store);
    let writer = File::create(home.join("lock")).unwrap();
    writer.try_lock().unwrap();
    fs::write(home.join("status.txt"), "1970-01-12T13:46:40Z\nunits \n").unwrap();
    let shown = status(dir.path()).unwrap();
    assert!(
        shown.contains("control  stopped\nwriter   running\nage "),
        "{shown}"
    );
}

#[test]
fn a_missing_pump_table_is_a_config_error() {
    let dir = mailspace_project("name = \"np\"\n");
    let err = run(dir.path(), true).unwrap_err();
    assert!(err.to_string().contains("no [pump] table"), "{err}");
}

#[test]
fn a_state_file_from_a_previous_run_is_loaded_and_saved_back() {
    let (dir, ctx, _host) = setup();
    let path = dir.path().join("home/state.json");
    let mut st = State::default();
    put(&mut st, "old-1", unit(Status::Merged, Some("9d98468e")));
    st.merges = 2;
    st.save(&path).unwrap();
    run_with(&ctx, true).unwrap();
    assert!(dir.path().join("home/state.json.imported").is_file());
    let back = PumpStore::open(&dir.path().join("home").join(DB_FILENAME))
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(back.units.get("old-1").unwrap().status, Status::Merged);
    assert!(back.order.iter().any(|uid| uid == "old-1"));
    assert_eq!(back.merges, 2);
}

#[test]
fn sqlite_startup_leaves_a_saved_merging_unit_as_it_was() {
    let (_dir, ctx, _host) = setup();
    let path = ctx.home.join(DB_FILENAME);
    let mut st = State::default();
    let mut abandoned = unit(Status::Merging, Some("task-1"));
    abandoned.branch = Some(String::from("unit/recover-1"));
    put(&mut st, "recover-1", abandoned);
    put(&mut st, "done-1", unit(Status::Merged, None));
    let mut store = PumpStore::open(&path).unwrap();
    store.save(&st).unwrap();
    assert_eq!(
        store.load().unwrap().units["recover-1"].status,
        Status::Merging
    );
    drop(store);

    run_with(&ctx, true).unwrap();
    let recovered = PumpStore::open(&path).unwrap().load().unwrap();
    assert_eq!(recovered.units["recover-1"].status, Status::Merging);
    assert_eq!(recovered.units["done-1"].status, Status::Merged);
    assert_eq!(recovered.units["recover-1"].task.as_deref(), Some("task-1"));
    assert_eq!(
        recovered.units["recover-1"].branch,
        st.units["recover-1"].branch
    );
}

#[test]
fn the_live_state_snapshot_imports_into_pump_db() {
    let (dir, ctx, _host) = setup();
    let snapshot = include_str!("state_snapshot.json");
    fs::write(ctx.home.join("state.json"), snapshot).unwrap();
    run_with(&ctx, true).unwrap();
    let want: State = serde_json::from_str(snapshot).unwrap();
    let back = PumpStore::open(&dir.path().join("home").join(DB_FILENAME))
        .unwrap()
        .load()
        .unwrap();
    assert_eq!(back.units.len(), want.units.len());
    assert_eq!(back.needs.len(), want.needs.len());
    assert!(back.units.values().all(|unit| unit.revision >= 1));
    assert!(back.needs.values().all(|need| need.revision >= 1));
    assert!(ctx.home.join("state.json.imported").is_file());
}

#[test]
fn a_missing_clone_without_a_remote_url_refuses_to_start() {
    let (dir, ctx, host) = setup();
    fs::remove_dir_all(&ctx.repo).unwrap();
    let err = run_with(&ctx, true).unwrap_err();
    assert!(err.to_string().contains("remote_url"), "{err}");
    assert!(!ctx.repo.exists());
    assert!(
        host.commands().is_empty(),
        "nothing ran: {:?}",
        host.commands()
    );
    drop(dir);
}

#[test]
fn a_missing_clone_is_cloned_from_the_remote_url() {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        remote_url: Some(String::from("git@example.com:org/repo.git")),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    fs::remove_dir_all(&ctx.repo).unwrap();
    crate::pump::run::ensure_repo(&ctx).unwrap();
    let clone = host.commands().into_iter().next().unwrap();
    assert!(
        clone.starts_with("git clone -q -o origin --branch main git@example.com:org/repo.git "),
        "{clone}"
    );
    assert!(
        clone.ends_with(&ctx.repo.to_string_lossy().into_owned()),
        "{clone}"
    );
}

#[test]
fn named_targets_initialize_separate_integration_clones_under_the_pump_home() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("project/radix");
    fs::create_dir_all(&source).unwrap();
    let mut cfg = PumpConfig::default();
    cfg.targets.insert(
        String::from("radix"),
        PumpTarget {
            path: String::from("radix"),
            ..PumpTarget::default()
        },
    );
    let (ctx, host) = test_ctx(dir.path(), cfg.clone());
    ensure_repo(&ctx).unwrap();
    let target_repo = cfg.target_repo_dir(&ctx.project, "radix").unwrap();
    let command = host
        .commands()
        .into_iter()
        .find(|command| command.contains("git clone"))
        .unwrap();
    assert!(command.contains(&target_repo.display().to_string()));
    assert!(command.contains(&source.display().to_string()));
    assert!(command.contains("--branch main"));
}

#[test]
fn a_failed_clone_stops_the_start_with_the_git_output() {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        remote_url: Some(String::from("/nowhere")),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    fs::remove_dir_all(&ctx.repo).unwrap();
    host.reply("git clone", 128, "fatal: repository not found");
    let err = crate::pump::run::ensure_repo(&ctx).unwrap_err();
    assert!(err.to_string().contains("repository not found"), "{err}");
}

#[test]
fn an_existing_clone_is_left_alone() {
    let (_dir, ctx, host) = setup();
    crate::pump::run::ensure_repo(&ctx).unwrap();
    assert!(host.commands().is_empty());
}

/// The dispatch world of the lifecycle tests: one `m1` lane and the `plain`
/// kind on pool `p1`, a one-second tick, and the board pass enabled and
/// pulling so it has a `sync pull` to park on.
fn dispatch_config() -> PumpConfig {
    PumpConfig {
        tick: 1,
        board: BoardConfig {
            from: Vec::new(),
            pull: true,
            ..BoardConfig::default()
        },
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: String::from("m1"),
                cap: 5,
            }],
            max_attempts: 3,
            kinds: std::collections::BTreeMap::from([(
                String::from("plain"),
                Kind {
                    pools: vec![String::from("p1")],
                    ..Kind::default()
                },
            )]),
            default_kind: String::from("plain"),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// A host over the same [`FakeHost`] that parks every `sync pull` until the
/// test releases it, so a board pass can be held open. The 30-second wait
/// cap keeps a regression from hanging the suite.
struct ParkHost {
    inner: Arc<FakeHost>,
    state: Mutex<(bool, bool)>,
    released: Condvar,
}

impl ParkHost {
    fn new(inner: Arc<FakeHost>) -> Arc<ParkHost> {
        Arc::new(ParkHost {
            inner,
            state: Mutex::new((false, false)),
            released: Condvar::new(),
        })
    }

    /// True while a `sync pull` is parked and unreleased.
    fn arrived(&self) -> bool {
        self.lock().0
    }

    /// Let the parked `sync pull` finish.
    fn release(&self) {
        let mut guard = self.lock();
        guard.1 = true;
        self.released.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (bool, bool)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn park(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut guard = self.lock();
        guard.0 = true;
        while !guard.1 && Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            guard = self
                .released
                .wait_timeout(guard, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        guard.0 = false;
    }
}

impl Host for ParkHost {
    fn now(&self) -> u64 {
        self.inner.now()
    }

    fn run(&self, cmd: &str, cwd: Option<&Path>, timeout_secs: u64) -> CmdOutput {
        if cmd.contains("sync pull") {
            self.park();
        }
        self.inner.run(cmd, cwd, timeout_secs)
    }

    fn spawn(
        &self,
        cmd: &str,
        cwd: Option<&Path>,
        stdin: Option<&Path>,
        out: &Path,
    ) -> Option<u32> {
        self.inner.spawn(cmd, cwd, stdin, out)
    }

    fn log(&self, line: &str) {
        self.inner.log(line);
    }
}

/// `ctx` with its host wrapped in a [`ParkHost`] over the same [`FakeHost`].
fn parked(ctx: &Ctx, host: &Arc<FakeHost>) -> (Ctx, Arc<ParkHost>) {
    let park = ParkHost::new(host.clone());
    let ctx = Ctx::with_parts(
        (*ctx.cfg).clone(),
        park.clone(),
        ctx.project.clone(),
        ctx.vivi.clone(),
        ctx.shell_prefix.clone(),
        ctx.home.clone(),
        ctx.repo.clone(),
    );
    (ctx, park)
}

/// Wait until the parked pass arrives, or fail the test.
fn wait_until_arrived(park: &ParkHost) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !park.arrived() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(park.arrived(), "the board pass never reached sync pull");
}

#[test]
fn a_blocked_board_pass_does_not_block_collect_or_the_tick() {
    let dir = tempdir().unwrap();
    let (base, host) = test_ctx(dir.path(), dispatch_config());
    let (ctx, park) = parked(&base, &host);
    host.reply(
        "pool reconcile",
        0,
        "collected 1a2b3c4d bridge slot-1 running \"agent pid 7\"",
    );
    let mut st = State::default();
    let mut job = planner_job("j-1", JobStatus::Dispatched, ctx.now(), Some(ctx.now()));
    job.task = Some(String::from("1a2b3c4d"));
    st.jobs.insert(String::from("j-1"), job);
    let mut pass = BoardPass::start(&ctx);
    pass.request(&ctx, &mut st);
    wait_until_arrived(&park);

    run_tick(&ctx, &mut st, &mut pass).unwrap();

    assert!(
        host.commands()
            .iter()
            .any(|cmd| cmd.contains("pool reconcile --porcelain")),
        "no collect while the pass was parked: {:?}",
        host.commands()
    );
    assert!(
        park.arrived(),
        "the tick was not observed while the pass was still parked"
    );

    park.release();
    let deadline = Instant::now() + Duration::from_secs(10);
    while st.board.at == 0 && Instant::now() < deadline {
        pass.apply(&ctx, &mut st);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(st.board.at > 0, "the finished pass never landed");
}

#[test]
fn stop_takes_effect_within_one_tick_while_a_board_pass_runs() {
    let dir = tempdir().unwrap();
    let (base, host) = test_ctx(dir.path(), dispatch_config());
    let (ctx, park) = parked(&base, &host);
    // The run loop only reads an initialized `pump_control` row.
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    drop(store);
    let (done, outcome) = std::sync::mpsc::channel();
    let runner = ctx.clone();
    let handle = thread::spawn(move || {
        let result = run_with(&runner, false);
        let _ = done.send(result);
    });
    wait_until_arrived(&park);
    thread::sleep(Duration::from_millis(500));
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.set_control_mode("stopped").unwrap();
    drop(store);

    let result = outcome
        .recv_timeout(Duration::from_secs(10))
        .expect("the run loop did not return within 10s of stop");
    assert!(result.is_ok(), "{:?}", result.err());
    park.release();
    drop(handle.join());
}

#[test]
fn actor_string_names_prefix_and_sync_name() {
    let dir = mailspace_project(
        "name = \"p\"\n\
         id = \"aeff60b0-1111-2222-3333-444444444444\"\n\
         \n[sync]\n\
         server = \"https://s\"\n\
         project = \"p\"\n\
         name = \"burgus\"\n",
    );
    let mailspace = Mailspace::discover(Some(dir.path())).unwrap();
    assert_eq!(actor_string(&mailspace), "aeff60b0/burgus");
}

#[test]
fn actor_string_without_sync_names_the_prefix_alone() {
    let dir = mailspace_project("name = \"p\"\nid = \"aeff60b0-1111-2222-3333-444444444444\"\n");
    let mailspace = Mailspace::discover(Some(dir.path())).unwrap();
    assert_eq!(actor_string(&mailspace), "aeff60b0");
}

#[test]
fn actor_string_without_a_mailspace_id_is_unknown() {
    let dir = mailspace_project("name = \"p\"\n");
    let mut mailspace = Mailspace::discover(Some(dir.path())).unwrap();
    // `discover` mints an id when the config lacks one; drop it to reach the
    // id-less case, which a hand-built mailspace would present.
    mailspace.config.id = None;
    assert_eq!(actor_string(&mailspace), "unknown");
}

#[test]
fn contexts_built_through_with_parts_carry_an_empty_actor() {
    let (dir, ctx, _host) = setup();
    assert_eq!(ctx.actor, "", "{dir:?}");
}

/// A running pump on its own thread with a one-second tick and a state that
/// already holds a pending job. Tests change pump.db through other handles
/// while the loop waits between ticks.
struct LiveLoop {
    home: std::path::PathBuf,
    done: std::sync::mpsc::Receiver<Result<(), crate::error::VivariumError>>,
    _thread: thread::JoinHandle<()>,
}

fn start_loop(ctx: &Ctx, held: &str) -> LiveLoop {
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    let mut st = State::default();
    let job = planner_job(held, JobStatus::Pending, ctx.now(), None);
    st.jobs.insert(held.to_owned(), job);
    store.save(&st).unwrap();
    store.initialize_control("running").unwrap();
    drop(store);
    let (done, outcome) = std::sync::mpsc::channel();
    let runner = ctx.clone();
    let thread = thread::spawn(move || {
        let result = run_with(&runner, false);
        drop(done.send(result));
    });
    LiveLoop {
        home: ctx.home.clone(),
        done: outcome,
        _thread: thread,
    }
}

impl LiveLoop {
    /// Wait until `n` ticks have finished; the loop is then in its between-tick
    /// wait, which lasts about a second.
    fn wait_for_ticks(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let cycles = crate::pump::cycle::read(&self.home).unwrap();
            if cycles.len() >= n && cycles.iter().all(|c| c.is_finished()) {
                return;
            }
            assert!(Instant::now() < deadline, "tick {n} never finished");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn store(&self) -> PumpStore {
        PumpStore::open(&self.home.join(DB_FILENAME)).unwrap()
    }

    /// Stop the loop and wait for it to return cleanly.
    fn stop(&self) {
        self.store().set_control_mode("stopped").unwrap();
        let result = self
            .done
            .recv_timeout(Duration::from_secs(10))
            .expect("the run loop did not return within 10s of stop");
        assert!(result.is_ok(), "{:?}", result.err());
    }

    fn job_ids(&self) -> Vec<String> {
        self.store().load().unwrap().jobs.into_keys().collect()
    }
}

fn one_second_tick_ctx(dir: &Path) -> Ctx {
    let cfg = PumpConfig {
        tick: 1,
        ..PumpConfig::default()
    };
    let (ctx, _host) = test_ctx(dir, cfg);
    ctx
}

#[test]
fn a_change_to_pump_db_between_ticks_is_seen_by_the_next_tick() {
    let dir = tempdir().unwrap();
    let ctx = one_second_tick_ctx(dir.path());
    let pump = start_loop(&ctx, "held-1");
    pump.wait_for_ticks(1);
    assert_eq!(pump.job_ids(), vec!["held-1"]);

    // Another handle removes the held job and adds a new one, with no restart.
    let mut store = pump.store();
    let mut st = store.load().unwrap();
    st.jobs.clear();
    let added = planner_job("added-2", JobStatus::Pending, ctx.now(), None);
    st.jobs.insert(String::from("added-2"), added);
    store.save(&st).unwrap();
    drop(store);

    pump.wait_for_ticks(2);
    assert_eq!(
        pump.job_ids(),
        vec!["added-2"],
        "the next tick acted on a state older than pump.db"
    );
    pump.stop();
    assert_eq!(pump.job_ids(), vec!["added-2"], "the final save went stale");
}

#[test]
fn stop_then_start_between_ticks_does_not_resurrect_removed_state() {
    let dir = tempdir().unwrap();
    let ctx = one_second_tick_ctx(dir.path());
    let pump = start_loop(&ctx, "held-1");
    pump.wait_for_ticks(1);

    // Faster than the loop's one-second control poll: stop, remove the job,
    // start again.
    let mut store = pump.store();
    store.set_control_mode("stopped").unwrap();
    let mut st = store.load().unwrap();
    st.jobs.clear();
    store.save(&st).unwrap();
    store.set_control_mode("running").unwrap();
    drop(store);

    pump.wait_for_ticks(2);
    assert!(
        pump.job_ids().is_empty(),
        "a removed job came back: {:?}",
        pump.job_ids()
    );
    pump.stop();
    assert!(
        pump.job_ids().is_empty(),
        "the final save resurrected a job"
    );
}
