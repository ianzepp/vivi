//! `vivi pump target list|add|remove` through the real binary.

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

fn init() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    assert!(
        vivi(&["mailspace", "init", "--project", root])
            .status
            .success()
    );
    let config = project.path().join(".vivi").join("mailspace.toml");
    let mut raw = fs::read_to_string(&config).unwrap();
    raw.push_str("\n[pools.p]\nprovider = \"local\"\nslots = 1\n\n[pump.rungs]\npools = [\"p\"]\n");
    fs::write(&config, raw).unwrap();
    project
}

fn config_path(project: &tempfile::TempDir) -> std::path::PathBuf {
    project.path().join(".vivi").join("mailspace.toml")
}

fn add_web(root: &str) -> Output {
    vivi(&[
        "pump",
        "target",
        "add",
        "web",
        "--path",
        "web",
        "--main",
        "trunk",
        "--rung",
        "task=npm test",
        "--merge",
        "remote_limit=600",
        "--merge",
        "format=npm run fmt",
        "--project",
        root,
    ])
}

#[test]
fn add_writes_the_target_and_list_shows_it() {
    let project = init();
    let root = project.path().to_str().unwrap();

    let empty = vivi(&["pump", "target", "list", "--project", root]);
    assert!(empty.status.success(), "{empty:?}");
    assert!(stdout(&empty).contains("no pump targets"), "{empty:?}");

    let added = add_web(root);
    assert!(added.status.success(), "{added:?}");
    assert!(stdout(&added).contains("added pump target web"));

    let raw = fs::read_to_string(config_path(&project)).unwrap();
    assert!(raw.contains("remote_limit = 600"), "{raw}");
    assert!(raw.contains("npm test"), "{raw}");

    let listed = vivi(&["pump", "target", "list", "--project", root]);
    assert!(listed.status.success(), "{listed:?}");
    assert!(
        stdout(&listed).contains("web path=web main=trunk rungs=task"),
        "{}",
        stdout(&listed)
    );
}

#[test]
fn add_of_an_existing_id_fails_and_leaves_the_file() {
    let project = init();
    let root = project.path().to_str().unwrap();
    assert!(add_web(root).status.success());
    let before = fs::read(config_path(&project)).unwrap();
    let again = add_web(root);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("already exists"), "{again:?}");
    assert_eq!(fs::read(config_path(&project)).unwrap(), before);
}

#[test]
fn add_with_a_bad_merge_value_is_rolled_back() {
    let project = init();
    let root = project.path().to_str().unwrap();
    let before = fs::read(config_path(&project)).unwrap();
    let out = vivi(&[
        "pump",
        "target",
        "add",
        "web",
        "--path",
        "web",
        "--merge",
        "remote_limit=abc",
        "--project",
        root,
    ]);
    assert!(!out.status.success(), "{out:?}");
    assert!(stderr(&out).contains("rolled back"), "{}", stderr(&out));
    assert_eq!(fs::read(config_path(&project)).unwrap(), before);
}

#[test]
fn remove_deletes_the_target_and_a_second_remove_fails() {
    let project = init();
    let root = project.path().to_str().unwrap();
    assert!(add_web(root).status.success());

    let removed = vivi(&["pump", "target", "remove", "web", "--project", root]);
    assert!(removed.status.success(), "{removed:?}");
    assert!(
        stdout(&removed).contains("a pump that is already up keeps it until it restarts"),
        "{}",
        stdout(&removed)
    );
    let listed = vivi(&["pump", "target", "list", "--project", root]);
    assert!(stdout(&listed).contains("no pump targets"));

    let again = vivi(&["pump", "target", "remove", "web", "--project", root]);
    assert!(!again.status.success());
    assert!(
        stderr(&again).contains("unknown pump target"),
        "{}",
        stderr(&again)
    );
}

#[test]
fn add_is_refused_while_the_pump_runs() {
    let project = init();
    let root = project.path().to_str().unwrap();
    assert!(vivi(&["pump", "pause", "--project", root]).status.success());
    assert!(
        vivi(&["pump", "resume", "--project", root])
            .status
            .success()
    );
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(project.path().join(".vivi/pump/lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let before = fs::read(config_path(&project)).unwrap();
    let out = add_web(root);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains("the pump is running"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fs::read(config_path(&project)).unwrap(), before);
    drop(lock);
}
