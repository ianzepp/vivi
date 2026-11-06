//! `role add`, `role set`, `role pool` and `role charter set` run under the
//! config guard: a write that adds an error is rolled back, and every write is
//! refused while the pump runs. A pid/host-only `role set` is runtime state and
//! skips the guard.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(project: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .arg("--project")
        .arg(project)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
    stdout(output)
}

fn refused(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        stdout(output)
    );
    stderr(output)
}

fn project() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    ok(&run(project.path(), &["mailspace", "init"]));
    project
}

fn config_path(project: &Path) -> PathBuf {
    project.join(".vivi/mailspace.toml")
}

fn backups(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".bak."))
        .collect();
    names.sort();
    names
}

fn hold_lock(root: &Path) -> File {
    let home = root.join(".vivi/pump");
    fs::create_dir_all(&home).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    lock
}

#[test]
fn a_role_pool_naming_an_unknown_pool_is_rolled_back() {
    let project = project();
    let before = fs::read(config_path(project.path())).unwrap();
    let err = refused(&run(project.path(), &["role", "pool", "merge", "nosuch"]));
    assert!(err.contains("rolled back"), "{err}");
    assert!(err.contains("names unknown pool `nosuch`"), "{err}");
    assert_eq!(fs::read(config_path(project.path())).unwrap(), before);
}

#[test]
fn a_charter_with_no_envelope_on_a_pump_role_is_rolled_back() {
    let project = project();
    let charter = project.path().join(".vivi/charters/merge.md");
    let before = fs::read(&charter).unwrap();
    let err = refused(&run(
        project.path(),
        &["role", "charter", "set", "merge", "--body", "plain text"],
    ));
    assert!(err.contains("rolled back"), "{err}");
    assert_eq!(fs::read(&charter).unwrap(), before);
}

#[test]
fn a_harmless_role_set_succeeds_and_names_a_backup() {
    let project = project();
    let out = run(
        project.path(),
        &["role", "set", "operator", "--status", "active"],
    );
    ok(&out);
    assert!(stderr(&out).contains("backup"), "{}", stderr(&out));
    assert_eq!(backups(&project.path().join(".vivi")).len(), 1);
}

#[test]
fn a_role_set_is_refused_while_the_pump_runs_and_allowed_while_paused() {
    let project = project();
    let root = project.path();
    ok(&run(root, &["pump", "pause"]));
    let lock = hold_lock(root);
    ok(&run(root, &["role", "set", "operator", "--model", "P2"]));
    drop(lock);

    ok(&run(root, &["pump", "resume"]));
    let _lock = hold_lock(root);
    let before = fs::read(config_path(root)).unwrap();
    let err = refused(&run(root, &["role", "set", "operator", "--model", "P1"]));
    assert!(err.contains("the pump is running"), "{err}");
    assert_eq!(fs::read(config_path(root)).unwrap(), before);
}

#[test]
fn a_pid_only_role_set_skips_the_guard_while_the_pump_runs() {
    let project = project();
    let root = project.path();
    ok(&run(root, &["pump", "resume"]));
    let _lock = hold_lock(root);
    let pid = std::process::id().to_string();
    ok(&run(root, &["role", "set", "operator", "--pid", &pid]));
    assert!(backups(&root.join(".vivi")).is_empty());
    let shown = ok(&run(root, &["role", "show", "operator", "--json"]));
    let view: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(view["pid"], std::process::id());

    ok(&run(root, &["role", "set", "operator", "--clear-pid"]));
    assert!(backups(&root.join(".vivi")).is_empty());
    let shown = ok(&run(root, &["role", "show", "operator", "--json"]));
    let view: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert!(view["pid"].is_null(), "{view}");

    let err = refused(&run(
        root,
        &["role", "set", "operator", "--pid", &pid, "--model", "P2"],
    ));
    assert!(err.contains("the pump is running"), "{err}");
}

fn role_kind(project: &Path, name: &str) -> serde_json::Value {
    let shown = ok(&run(project, &["role", "show", name, "--json"]));
    let view: serde_json::Value = serde_json::from_str(&shown).unwrap();
    view["kind"].clone()
}

#[test]
fn role_add_of_the_pump_hand_role_defaults_to_kind_hand() {
    let project = project();
    let root = project.path();
    ok(&run(root, &["role", "add", "hand"]));
    assert_eq!(role_kind(root, "hand"), "hand");
}

#[test]
fn an_explicit_kind_beats_the_hand_default() {
    let project = project();
    let root = project.path();
    // `auditor` for the pump's hand role fails validation, so the guard rolls
    // the write back; the message naming kind auditor shows `--kind` won.
    let before = fs::read(config_path(root)).unwrap();
    let err = refused(&run(root, &["role", "add", "hand", "--kind", "auditor"]));
    assert!(err.contains("rolled back"), "{err}");
    assert!(err.contains("pump role hand has kind auditor"), "{err}");
    assert_eq!(fs::read(config_path(root)).unwrap(), before);
}

#[test]
fn role_add_of_another_name_gets_no_default_kind() {
    let project = project();
    let root = project.path();
    ok(&run(root, &["role", "add", "mind"]));
    assert!(role_kind(root, "mind").is_null());
}

fn role_view(project: &Path, name: &str) -> serde_json::Value {
    let shown = ok(&run(project, &["role", "show", name, "--json"]));
    serde_json::from_str(&shown).unwrap()
}

fn role_field(project: &Path, name: &str, field: &str) -> serde_json::Value {
    role_view(project, name)[field].clone()
}

#[test]
fn role_add_exposes_the_command_and_target_in_show_and_list() {
    let project = project();
    let root = project.path();
    let script = "set -e\nprintf '%s\\n' \"hello world\"\n";
    ok(&run(
        root,
        &[
            "role",
            "add",
            "hand-1",
            "--command",
            script,
            "--target",
            "default",
        ],
    ));
    assert_eq!(role_field(root, "hand-1", "command"), script);
    assert_eq!(role_field(root, "hand-1", "target"), "default");

    let listed = ok(&run(root, &["role", "list", "--json"]));
    let views: serde_json::Value = serde_json::from_str(&listed).unwrap();
    let entry = views
        .as_array()
        .unwrap()
        .iter()
        .find(|view| view["name"] == "hand-1")
        .expect("hand-1 is listed");
    assert_eq!(entry["command"], script);
    assert_eq!(entry["target"], "default");

    let text = ok(&run(root, &["role", "show", "hand-1"]));
    assert!(text.contains("command:"), "{text}");
    assert!(text.contains("hello world"), "{text}");
    assert!(text.contains("target:    default"), "{text}");
}

#[test]
fn a_command_only_and_a_target_only_set_are_real_guarded_writes() {
    let project = project();
    let root = project.path();
    let script = "printf '%s\\n' one\nprintf '%s\\n' two\n";
    let out = run(root, &["role", "set", "operator", "--command", script]);
    ok(&out);
    assert!(stderr(&out).contains("backup"), "{}", stderr(&out));
    assert_eq!(role_field(root, "operator", "command"), script);
    assert_eq!(backups(&root.join(".vivi")).len(), 1);

    ok(&run(
        root,
        &["role", "set", "operator", "--target", "default"],
    ));
    assert_eq!(role_field(root, "operator", "target"), "default");
    assert_eq!(role_field(root, "operator", "command"), script);
    assert_eq!(backups(&root.join(".vivi")).len(), 2);
}

#[test]
fn clearing_the_command_and_target_wins_over_setting_them() {
    let project = project();
    let root = project.path();
    ok(&run(
        root,
        &[
            "role",
            "set",
            "operator",
            "--command",
            "true",
            "--target",
            "default",
        ],
    ));
    assert_eq!(role_field(root, "operator", "command"), "true");

    ok(&run(
        root,
        &[
            "role",
            "set",
            "operator",
            "--command",
            "false",
            "--clear-command",
            "--target",
            "default",
            "--clear-target",
        ],
    ));
    let view = role_view(root, "operator");
    assert!(view["command"].is_null(), "{view}");
    assert!(view["target"].is_null(), "{view}");

    let text = ok(&run(root, &["role", "show", "operator"]));
    assert!(text.contains("command:   (unset)"), "{text}");
    assert!(text.contains("target:    (unset)"), "{text}");
}

#[test]
fn an_unknown_target_is_refused_and_changes_nothing() {
    let project = project();
    let root = project.path();
    let before = fs::read(config_path(root)).unwrap();
    let err = refused(&run(
        root,
        &["role", "set", "operator", "--target", "nosuch"],
    ));
    assert!(!err.is_empty(), "{err}");
    assert_eq!(fs::read(config_path(root)).unwrap(), before);
    assert!(role_field(root, "operator", "target").is_null());

    let before = fs::read(config_path(root)).unwrap();
    refused(&run(root, &["role", "add", "hand-1", "--target", "nosuch"]));
    assert_eq!(fs::read(config_path(root)).unwrap(), before);
}

#[test]
fn command_and_target_updates_are_refused_while_the_pump_runs() {
    let project = project();
    let root = project.path();
    ok(&run(root, &["pump", "resume"]));
    let _lock = hold_lock(root);
    let before = fs::read(config_path(root)).unwrap();

    let err = refused(&run(
        root,
        &["role", "set", "operator", "--command", "true"],
    ));
    assert!(err.contains("the pump is running"), "{err}");
    let err = refused(&run(
        root,
        &["role", "set", "operator", "--target", "default"],
    ));
    assert!(err.contains("the pump is running"), "{err}");
    let err = refused(&run(root, &["role", "set", "operator", "--clear-command"]));
    assert!(err.contains("the pump is running"), "{err}");
    let err = refused(&run(root, &["role", "set", "operator", "--clear-target"]));
    assert!(err.contains("the pump is running"), "{err}");

    // A mixed pid/command update is configuration, so the guard applies.
    let pid = std::process::id().to_string();
    let err = refused(&run(
        root,
        &[
            "role",
            "set",
            "operator",
            "--pid",
            &pid,
            "--command",
            "true",
        ],
    ));
    assert!(err.contains("the pump is running"), "{err}");
    assert_eq!(fs::read(config_path(root)).unwrap(), before);

    // The pid/host-only exception is retained.
    ok(&run(root, &["role", "set", "operator", "--pid", &pid]));
    assert!(backups(&root.join(".vivi")).is_empty());
}
