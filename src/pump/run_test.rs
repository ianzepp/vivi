//! Tests for [`crate::pump::run`]: the status file, the tick order, the
//! flags, the lock, and the run/status entry points.

use std::fs::{self, File};
use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::Merger;
use crate::pump::run::{run, run_with, status, tick, write_status};
use crate::pump::state::{GateRun, NeedRecord, State, Status, Unit};

/// The fake clock's start (1970-01-12T13:46:40Z).
const T0: u64 = 1_000_000;

/// A mailspace whose `[pump]` table passes `validate`: the default
/// `board.mark` is set, so `planner.run` must be non-empty.
const PUMP_TOML: &str = concat!(
    "name = \"pump\"\n\n",
    "[pump]\nbranch = \"next\"\ntick = 1\n",
    "committer_name = \"Pump\"\ncommitter_email = \"pump@example.com\"\n\n",
    "[pump.merge]\nshared_checks = [\"true\"]\n\n[pump.gate]\ncommand = \"true\"\n\n",
    "[pump.dispatch]\nlanes = [{ model = \"m\", cap = 2 }]\n\n",
    "[pump.planner]\nrun = \"planner-model\"\n",
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

/// The first index of a command line containing `fragment`.
fn first_at(commands: &[String], fragment: &str) -> usize {
    commands
        .iter()
        .position(|command| command.contains(fragment))
        .unwrap_or(usize::MAX)
}

#[test]
fn write_status_writes_four_lines_with_counts() {
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
    st.gates.push(crate::pump::state::GateRecord::default());
    write_status(&ctx, &st);
    let text = fs::read_to_string(dir.path().join("home/status.txt")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], "1970-01-12T13:46:40Z");
    assert_eq!(lines[1], "units merged=1 pending=2");
    assert_eq!(lines[2], "plans closed=2 units=1");
    assert_eq!(lines[3], "merges 3  needs-ready 2  gates 1  baseline none");
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
    put(&mut st, "a-1", unit(Status::Inflight, Some("1a2b3c4d")));
    let (merger, _results, _jobs) = Merger::detached();
    tick(&ctx, &mut st, &merger);
    let commands = host.commands();
    let sync = first_at(&commands, "sync pull");
    let reconcile = first_at(&commands, "pool reconcile --porcelain");
    let pool_status = first_at(&commands, "pool status --porcelain");
    assert!(sync < usize::MAX, "no sync pull in {commands:?}");
    assert!(reconcile < usize::MAX, "no pool reconcile in {commands:?}");
    assert!(pool_status < usize::MAX, "no pool status in {commands:?}");
    assert!(sync < reconcile && reconcile < pool_status);
}

#[test]
fn a_paused_tick_dispatches_nothing_and_still_writes_status() {
    let (dir, ctx, host) = setup();
    fs::write(ctx.home.join("PAUSE"), b"").unwrap();
    let mut st = State::default();
    let (merger, _results, _jobs) = Merger::detached();
    tick(&ctx, &mut st, &merger);
    let commands = host.commands();
    assert!(!commands.iter().any(|c| c.contains("pool status")));
    assert!(!commands.iter().any(|c| c.contains("task send")));
    let text = fs::read_to_string(dir.path().join("home/status.txt")).unwrap();
    assert_eq!(text.lines().count(), 4);
}

#[test]
fn a_finished_gate_task_reaches_finish_gate_through_collect() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool reconcile",
        0,
        "collected g1a2b3c4d bridge slot-1 finished \"gate done\"",
    );
    host.reply(
        "task show g1a2b3c4d",
        0,
        "gate reply\nFAILING-BEGIN\ncrate::a::t\nFAILING-END\n",
    );
    let mut st = State {
        baseline: None,
        last_gate: GateRun {
            at: T0,
            merges: 0,
            task: Some(String::from("g1a2b3c4d")),
            tip: Some(String::from("0123456789abcdef0123456789abcdef01234567")),
            pool: Some(String::from("flygate")),
        },
        ..State::default()
    };
    let (merger, _results, _jobs) = Merger::detached();
    tick(&ctx, &mut st, &merger);
    assert_eq!(st.baseline, Some(vec![String::from("crate::a::t")]));
    assert_eq!(st.gates.len(), 1);
    assert_eq!(st.last_gate.task, None);
    assert!(
        host.commands()
            .iter()
            .any(|c| c.contains("task show g1a2b3c4d"))
    );
}

#[test]
fn a_second_instance_is_refused_by_the_lock() {
    let dir = mailspace_project(PUMP_TOML);
    let home = dir.path().join(".vivi").join("pump");
    fs::create_dir_all(&home).unwrap();
    let first = File::create(home.join("lock")).unwrap();
    first.try_lock().unwrap();
    let err = run(dir.path(), true).unwrap_err();
    assert!(err.to_string().contains("another vivi pump is running"));
}

#[test]
fn once_runs_exactly_one_tick() {
    let (dir, ctx, host) = setup();
    run_with(&ctx, true).unwrap();
    // One tick makes exactly two `pool status --porcelain` calls, as in
    // pump.py: the dispatch slot count and the gate's pool pick. One tick
    // is two; a second tick would make four.
    let statuses = host
        .commands()
        .iter()
        .filter(|c| c.contains("pool status --porcelain"))
        .count();
    assert_eq!(statuses, 2);
    assert!(dir.path().join("home/state.json").is_file());
}

#[test]
fn the_stop_flag_ends_the_run_loop_after_a_tick() {
    let (dir, ctx, host) = setup();
    fs::write(ctx.home.join("STOP"), b"").unwrap();
    run_with(&ctx, false).unwrap();
    // One tick makes exactly two `pool status --porcelain` calls (see
    // `once_runs_exactly_one_tick`); a second tick would make four.
    let statuses = host
        .commands()
        .iter()
        .filter(|c| c.contains("pool status --porcelain"))
        .count();
    assert_eq!(statuses, 2);
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("STOP file seen; exiting"))
    );
    assert!(dir.path().join("home/state.json").is_file());
}

#[test]
fn status_reads_the_status_file_or_says_no_status_yet() {
    let with_file = mailspace_project("name = \"s\"\n\n[pump]\nhome = \"pump-home\"\n");
    let home = with_file.path().join("pump-home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("status.txt"), "1970-01-12T13:46:40Z\nunits \n").unwrap();
    assert_eq!(
        status(with_file.path()).unwrap(),
        "1970-01-12T13:46:40Z\nunits \n"
    );
    let without_file = mailspace_project("name = \"s\"\n\n[pump]\nhome = \"pump-home\"\n");
    assert_eq!(status(without_file.path()).unwrap(), "no status yet");
    let no_mailspace = tempdir().unwrap();
    assert_eq!(status(no_mailspace.path()).unwrap(), "no status yet");
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
    let back = State::load(&path).unwrap();
    assert_eq!(back.units.get("old-1").unwrap().status, Status::Merged);
    assert!(back.order.iter().any(|uid| uid == "old-1"));
    assert_eq!(back.merges, 2);
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
        branch: String::from("next"),
        remote_url: Some(String::from("git@example.com:org/repo.git")),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    fs::remove_dir_all(&ctx.repo).unwrap();
    crate::pump::run::ensure_repo(&ctx).unwrap();
    let clone = host.commands().into_iter().next().unwrap();
    assert!(
        clone.starts_with("git clone -q -o origin --branch next git@example.com:org/repo.git "),
        "{clone}"
    );
    assert!(
        clone.ends_with(&ctx.repo.to_string_lossy().into_owned()),
        "{clone}"
    );
}

#[test]
fn a_failed_clone_stops_the_start_with_the_git_output() {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
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
