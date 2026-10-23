use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::mailspace::Mailspace;
use crate::pump::{control, run};

/// A project exactly as `vivi mailspace init` leaves it.
fn fresh_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.ensure_operator_role().unwrap();
    space.ensure_merge_role().unwrap();
    space.ensure_pump_planner_role().unwrap();
    space.ensure_starter_pump().unwrap();
    tmp
}

fn home(project: &Path) -> std::path::PathBuf {
    project.join(".vivi").join("pump")
}

#[test]
fn pump_once_on_a_not_ready_project_reports_and_returns_ok() {
    let tmp = fresh_project();
    run::run(tmp.path(), true).expect("a not-ready project is held, not an error");
    let status = fs::read_to_string(home(tmp.path()).join("status.txt")).unwrap();
    assert!(status.contains("not ready:"), "{status}");
    assert!(status.contains("pump.committer_name"), "{status}");
    assert!(status.contains("nothing is dispatched"), "{status}");
    let log = fs::read_to_string(home(tmp.path()).join("log.txt")).unwrap();
    assert!(log.contains("pump holding: not ready:"), "{log}");
}

#[test]
fn a_held_pump_dispatches_nothing_and_never_opens_the_repository() {
    let tmp = fresh_project();
    run::run(tmp.path(), true).unwrap();
    // No integration clone is made and no unit state is written.
    assert!(!home(tmp.path()).join("repo").exists());
    let status = run::status(tmp.path()).unwrap();
    assert!(status.contains("not ready:"), "{status}");
}

#[test]
fn a_held_pump_keeps_the_lock_and_exits_cleanly_on_a_stop_request() {
    let tmp = fresh_project();
    let root = tmp.path().to_path_buf();
    let worker = {
        let root = root.clone();
        thread::spawn(move || run::run(&root, false))
    };
    let status = home(&root).join("status.txt");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !status.exists() {
        assert!(Instant::now() < deadline, "the held pump wrote no status");
        thread::sleep(Duration::from_millis(50));
    }
    assert!(run::lock_is_held(&home(&root)).unwrap());
    control::request_shutdown(&home(&root), true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "the held pump did not stop");
        thread::sleep(Duration::from_millis(50));
    }
    worker
        .join()
        .unwrap()
        .expect("a stop request is a clean exit");
    assert!(!run::lock_is_held(&home(&root)).unwrap());
}

#[test]
fn a_stopped_held_pump_exits_without_waiting() {
    let tmp = fresh_project();
    let home = home(tmp.path());
    fs::create_dir_all(&home).unwrap();
    control::initialize(&home).unwrap();
    control::set_project_mode(tmp.path(), control::PumpMode::Stopped).unwrap();
    run::run(tmp.path(), false).expect("stopped is a clean exit");
}

const POOL: &str = "
[pools.p]
provider = \"local\"
slots = 1

[pools.p.harness]
name = \"h\"
run = \"h --model {model} @{brief}\"
models = [\"m\"]

[pools.p.bands]
P2 = [{ model = \"m\", reasoning = \"low\" }]
";

/// Everything a pump needs, written into a fresh project at once.
fn fill_in(root: &Path) {
    let path = root.join(".vivi").join("mailspace.toml");
    let text = fs::read_to_string(&path).unwrap();
    let text = text
        .replace("branch = \"\"", "branch = \"next\"")
        .replace("committer_name = \"\"", "committer_name = \"T\"")
        .replace(
            "committer_email = \"\"",
            "committer_email = \"t@example.com\"",
        )
        .replace("shared_checks = []", "shared_checks = [\"true\"]")
        .replace("command = \"\"", "command = \"true\"");
    fs::write(&path, text + POOL).unwrap();
    let mut space = Mailspace::open_root(root.to_path_buf()).unwrap();
    for (name, kind) in [
        ("hand", "hand"),
        ("merge", "merge"),
        ("pump-planner", "planner"),
        ("auditor", "auditor"),
        ("mind", "mind"),
    ] {
        let update = crate::mailspace::RoleUpdate {
            kind: Some(Some(kind.to_owned())),
            pool: Some(Some(String::from("p"))),
            model: Some(Some(String::from("P2"))),
            ..crate::mailspace::RoleUpdate::default()
        };
        if space.role_view(name).is_ok() {
            space.set_role(name, update).unwrap();
        } else {
            space.add_role(name, update).unwrap();
        }
    }
}

#[test]
fn a_held_pump_starts_for_real_once_the_last_item_is_supplied() {
    let tmp = fresh_project();
    let root = tmp.path().to_path_buf();
    let path = root.join(".vivi").join("mailspace.toml");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("[pump]\n", "[pump]\ntick = 1\n")).unwrap();
    let worker = {
        let root = root.clone();
        thread::spawn(move || run::run(&root, false))
    };
    let status = home(&root).join("status.txt");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !status.exists() {
        assert!(Instant::now() < deadline, "the held pump wrote no status");
        thread::sleep(Duration::from_millis(50));
    }
    fill_in(&root);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "the pump never left the hold");
        thread::sleep(Duration::from_millis(50));
    }
    // Past the hold the pump begins a real start, which needs the integration
    // clone this project does not have: that refusal proves it left the hold.
    let outcome = worker.join().unwrap();
    let error = outcome.expect_err("a ready pump needs its integration clone");
    assert!(error.to_string().contains("remote_url"), "{error}");
    let log = fs::read_to_string(home(&root).join("log.txt")).unwrap();
    assert!(log.contains("pump is ready; starting"), "{log}");
}
