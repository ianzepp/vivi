//! End-to-end smoke test of `vivi pump` through the real binary: `status`
//! before any tick, one tick of `once` over a temp mailspace, the files the
//! tick writes, the `PAUSE` flag, the missing `[pump]` table error, and the
//! single-instance lock.

use std::fs::{self, File};
use std::process::{Command, Output};

#[test]
fn status_before_any_tick_says_no_status_yet() {
    let project = temp_mailspace();
    let out = vivi(["pump", "status", "--project", root(&project)]);
    assert_success(&out);
    assert!(stdout(&out).contains("no status yet"), "{}", stdout(&out));
}

#[test]
fn once_runs_one_tick_and_writes_db_status_and_log() {
    let project = temp_mailspace();
    let home = pump_home(&project);
    // One verdict-only plan: ingest logs it (the tick's log event) without
    // minting units, so nothing is dispatched or merged.
    fs::create_dir_all(home.join("plans")).unwrap();
    fs::write(
        home.join("plans").join("need-1.json"),
        "{\"need\":\"need-1\",\"verdict\":\"hold\"}",
    )
    .unwrap();
    assert_success(&vivi(["pump", "once", "--project", root(&project)]));
    let status = fs::read_to_string(home.join("status.txt")).unwrap();
    assert!(home.join("pump.db").is_file(), "no pump.db");
    assert!(home.join("log.txt").is_file(), "no log.txt");
    let lines: Vec<&str> = status.lines().collect();
    assert_eq!(lines.len(), 4, "status.txt must be 4 lines: {status}");
    assert!(lines[1].starts_with("units"), "line 2: {status}");
    let shown = vivi(["pump", "status", "--project", root(&project)]);
    assert_success(&shown);
    assert_eq!(stdout(&shown), status, "status must print status.txt");
}

#[test]
fn export_writes_the_state_json_shape() {
    let project = temp_mailspace();
    let home = pump_home(&project);
    assert_success(&vivi(["pump", "once", "--project", root(&project)]));
    let out = project.path().join("export.json");
    let args = [
        "pump",
        "export",
        "--json",
        out.to_str().unwrap(),
        "--project",
        root(&project),
    ];
    assert_success(&vivi(args));
    let text = fs::read_to_string(&out).unwrap();
    assert!(text.contains("\"units\""), "{text}");
    assert!(home.join("pump.db").is_file());
}

#[test]
fn a_pause_flag_still_writes_status_and_exits_zero() {
    let project = temp_mailspace();
    let home = pump_home(&project);
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("PAUSE"), "").unwrap();
    assert_success(&vivi(["pump", "once", "--project", root(&project)]));
    let status = fs::read_to_string(home.join("status.txt")).unwrap();
    assert_eq!(status.lines().count(), 4, "{status}");
}

#[test]
fn a_missing_pump_table_is_a_clear_error() {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    let out = vivi(["pump", "once", "--project", root(&project)]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("[pump]"), "{}", stderr(&out));
}

#[test]
fn a_second_instance_is_refused() {
    let project = temp_mailspace();
    let home = pump_home(&project);
    fs::create_dir_all(&home).unwrap();
    let held = File::create(home.join("lock")).unwrap();
    held.lock().unwrap();
    let out = vivi(["pump", "once", "--project", root(&project)]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("another vivi pump is running"),
        "{}",
        stderr(&out)
    );
    drop(held);
    assert_success(&vivi(["pump", "once", "--project", root(&project)]));
}

/// A temp project mailspace with the minimal `[pump]` table appended.
fn temp_mailspace() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    let config = project.path().join(".vivi").join("mailspace.toml");
    let root = project.path().to_str().unwrap();
    let table = format!(
        "\n[pump]\nbranch = \"next\"\nhome = \"{root}/.vivi/pump-home\"\ntick = 1\n\
         committer_name = \"Test\"\ncommitter_email = \"test@example.com\"\n\n\
         [pump.board]\nmark = \"\"\nfrom = []\npull = false\n\n\
         [pump.dispatch]\nlanes = [{{ model = \"test/model\", cap = 1 }}]\n\n\
         [pump.merge]\nshared_checks = [\"true\"]\n\n\
         [pump.gate]\ncommand = \"true\"\n"
    );
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&table);
    fs::write(config, text).unwrap();
    // The pump refuses to start without its integration clone; an empty repository stands in.
    let repo = pump_home(&project).join("repo");
    fs::create_dir_all(&repo).unwrap();
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_success(&init);
    project
}

fn pump_home(project: &tempfile::TempDir) -> std::path::PathBuf {
    project.path().join(".vivi").join("pump-home")
}

fn root(project: &tempfile::TempDir) -> &str {
    project.path().to_str().unwrap()
}

fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
