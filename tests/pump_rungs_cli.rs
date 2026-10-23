//! `vivi pump rungs show|set` through the real binary.

use std::fs;
use std::process::{Command, Output};

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Give the project a pool and name it as the rung pool, so a rung command
/// has somewhere to run.
fn add_rung_pool(config: &std::path::Path) {
    let mut raw = fs::read_to_string(config).unwrap();
    raw.push_str("\n[pools.p]\nprovider = \"local\"\nslots = 1\n\n[pump.rungs]\npools = [\"p\"]\n");
    fs::write(config, raw).unwrap();
}

fn init() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    assert!(
        vivi(&["mailspace", "init", "--project", root])
            .status
            .success()
    );
    project
}

#[test]
fn rungs_set_then_show_round_trips_and_rejects_unknown_rung() {
    let project = init();
    let root = project.path().to_str().unwrap();
    let config = project.path().join(".vivi").join("mailspace.toml");
    add_rung_pool(&config);

    let shown = vivi(&["pump", "rungs", "show", "--project", root]);
    assert!(shown.status.success(), "{shown:?}");
    let text = stdout(&shown);
    assert!(text.contains("pools: p"), "{text}");
    for name in ["task", "need", "main", "release"] {
        assert!(text.contains(&format!("{name}: (unset)")), "{text}");
    }
    assert!(text.contains("timeout: 1800"), "{text}");

    let set = vivi(&[
        "pump",
        "rungs",
        "set",
        "task",
        "cargo test --quiet",
        "--project",
        root,
    ]);
    assert!(set.status.success(), "{set:?}");
    let text = stdout(&vivi(&["pump", "rungs", "show", "--project", root]));
    assert!(text.contains("task: cargo test --quiet"), "{text}");
    assert!(text.contains("need: (unset)"), "{text}");

    let before = fs::read_to_string(&config).unwrap();
    let unknown = vivi(&["pump", "rungs", "set", "bogus", "true", "--project", root]);
    assert!(!unknown.status.success());
    let blank = vivi(&["pump", "rungs", "set", "need", "  ", "--project", root]);
    assert!(!blank.status.success());
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn rungs_target_override_wins_and_unknown_target_is_refused() {
    let project = init();
    let root = project.path().to_str().unwrap();
    let config = project.path().join(".vivi").join("mailspace.toml");
    add_rung_pool(&config);

    let missing = vivi(&[
        "pump",
        "rungs",
        "set",
        "task",
        "true",
        "--target",
        "web",
        "--project",
        root,
    ]);
    assert!(!missing.status.success());

    let mut raw = fs::read_to_string(&config).unwrap();
    raw.push_str("\n[pump.targets.web]\npath = \"web\"\n");
    fs::write(&config, raw).unwrap();

    let set = vivi(&[
        "pump",
        "rungs",
        "set",
        "need",
        "make all",
        "--project",
        root,
    ]);
    assert!(set.status.success(), "{set:?}");
    let set = vivi(&[
        "pump",
        "rungs",
        "set",
        "task",
        "npm test",
        "--target",
        "web",
        "--project",
        root,
    ]);
    assert!(set.status.success(), "{set:?}");
    let text = stdout(&vivi(&[
        "pump",
        "rungs",
        "show",
        "--target",
        "web",
        "--project",
        root,
    ]));
    assert!(text.contains("task: npm test"), "{text}");
    assert!(text.contains("need: make all"), "{text}");
}

#[test]
fn rungs_set_without_pools_is_rolled_back() {
    let project = init();
    let root = project.path().to_str().unwrap();
    let config = project.path().join(".vivi").join("mailspace.toml");
    let before = fs::read(&config).unwrap();
    let out = vivi(&["pump", "rungs", "set", "task", "true", "--project", root]);
    assert!(!out.status.success(), "{out:?}");
    let err = stderr(&out);
    assert!(err.contains("rolled back"), "{err}");
    assert!(err.contains("effective pools are empty"), "{err}");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn rungs_set_is_refused_while_the_pump_runs() {
    let project = init();
    let root = project.path().to_str().unwrap();
    let config = project.path().join(".vivi").join("mailspace.toml");
    add_rung_pool(&config);
    assert!(vivi(&["pump", "pause", "--project", root]).status.success());
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(project.path().join(".vivi/pump/lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let paused = vivi(&["pump", "rungs", "set", "task", "true", "--project", root]);
    assert!(paused.status.success(), "{paused:?}");
    drop(lock);

    assert!(
        vivi(&["pump", "resume", "--project", root])
            .status
            .success()
    );
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(project.path().join(".vivi/pump/lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let before = fs::read(&config).unwrap();
    let out = vivi(&["pump", "rungs", "set", "need", "true", "--project", root]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains("the pump is running"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fs::read(&config).unwrap(), before);
    drop(lock);
}
