//! `vivi pump reset` through the real binary.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use vivi::mailspace::{Mailspace, PumpState};
use vivi::pump::control::{self, PumpMode};

/// Home entries `reset` renames, in archive order. Same list as the library.
const MOVED: [&str; 10] = [
    "pump.db",
    "pump.db-wal",
    "pump.db-shm",
    "state.json",
    "state.json.imported",
    "plans",
    "resplit",
    "reserved.txt",
    "exceptions.txt",
    "status.txt",
];

const NOTE: &str = "pump records are letters: the next tick re-ingests every open record and starts fresh runs; use vivi pump withdraw to stop a need instead";

#[test]
fn reset_on_a_stopped_pump_archives_entries_and_prints_the_path() {
    let project = temp_project();
    let root = root(&project);
    open_pump_record(&root);
    let home = pump_home(&project);
    seed_home(&home);

    let out = vivi(["pump", "reset", "--project", &root]);
    let text = ok(&out);
    let archive = PathBuf::from(text.lines().next().expect("archive path"));
    assert!(
        archive
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pump-archive-"),
        "{}",
        archive.display()
    );
    assert_eq!(archive.parent(), home.parent());
    assert_eq!(text, expected_text(&archive, 1));
    assert_eq!(open_records(&project), 1);

    for name in MOVED {
        assert!(archive.join(name).exists(), "{name} not archived");
        if name == "pump.db" {
            assert_eq!(fs::read(archive.join(name)).unwrap(), b"seed\n");
            assert!(home.join(name).is_file(), "no fresh pump.db");
            assert_ne!(fs::read(home.join(name)).unwrap(), b"seed\n");
        } else {
            assert!(!home.join(name).exists(), "{name} stayed in the home");
        }
    }
    assert!(archive.join("plans").join("need.json").is_file());
    for keep in ["targets", "merge-lanes", "sync-push.log", "log.txt"] {
        assert!(home.join(keep).exists(), "{keep} missing after reset");
    }
    assert_eq!(control::inspect(&home).unwrap().mode, PumpMode::Stopped);
    let log = fs::read_to_string(home.join("log.txt")).unwrap();
    assert!(
        log.contains(&format!("reset: archived to {}", archive.display())),
        "{log}"
    );
}

#[test]
fn reset_json_prints_the_same_facts() {
    let project = temp_project();
    let root = root(&project);
    open_pump_record(&root);
    let home = pump_home(&project);
    seed_home(&home);

    let out = vivi(["pump", "reset", "--json", "--project", &root]);
    let text = ok(&out);
    let value: Value = serde_json::from_str(&text).unwrap();
    let archive = PathBuf::from(value["archive_dir"].as_str().unwrap());
    assert!(archive.is_dir(), "{}", archive.display());
    assert_eq!(value["mode"], "stopped");
    assert_eq!(value["open_records"], 1);
    assert_eq!(value["note"], NOTE);
    let moved: Vec<&str> = value["moved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    assert_eq!(moved, MOVED);
    assert_eq!(control::inspect(&home).unwrap().mode, PumpMode::Stopped);
    assert_eq!(text, expected_json(&archive));
}

#[test]
fn reset_while_the_lock_is_held_exits_nonzero_and_changes_nothing() {
    let project = temp_project();
    let home = pump_home(&project);
    seed_home(&home);
    let lock = File::create(home.join("lock")).unwrap();
    lock.try_lock().unwrap();
    let before = snapshot(&home);

    let out = vivi(["pump", "reset", "--project", root(&project)]);

    assert!(!out.status.success(), "{}", stdout(&out));
    let error = stderr(&out);
    assert!(
        error.contains(
            "the pump is running (lock held): run vivi pump stop and wait for it to exit, then reset",
        ),
        "{error}"
    );
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert_eq!(snapshot(&home), before);
    assert!(archives(&home).is_empty(), "{:?}", archives(&home));
    drop(lock);
}

fn expected_text(archive: &Path, open: usize) -> String {
    let mut text = format!("{}\n", archive.display());
    for name in MOVED {
        text.push_str(&format!("moved {name}\n"));
    }
    text.push_str(&format!(
        "control: stopped\nopen pump records: {open}\n{NOTE}\n"
    ));
    text
}

/// Object keys are sorted: this crate's `serde_json` map is a `BTreeMap`.
fn expected_json(archive: &Path) -> String {
    format!(
        "{{\n  \"archive_dir\": \"{}\",\n  \"mode\": \"stopped\",\n  \"moved\": [\n    \"pump.db\",\n    \"pump.db-wal\",\n    \"pump.db-shm\",\n    \"state.json\",\n    \"state.json.imported\",\n    \"plans\",\n    \"resplit\",\n    \"reserved.txt\",\n    \"exceptions.txt\",\n    \"status.txt\"\n  ],\n  \"note\": \"{NOTE}\",\n  \"open_records\": 1\n}}\n",
        archive.display()
    )
}

fn open_records(project: &tempfile::TempDir) -> usize {
    Mailspace::discover(Some(project.path()))
        .unwrap()
        .pump_list(None, Some(PumpState::Open))
        .unwrap()
        .len()
}

fn temp_project() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    ok(&vivi(["mailspace", "init", "--project", root(&project)]));
    project
}

fn open_pump_record(root: &str) {
    ok(&vivi([
        "mailspace",
        "identity",
        "add",
        "mind",
        "--project",
        root,
    ]));
    let created = ok(&vivi([
        "need",
        "send",
        "--project",
        root,
        "--from",
        "operator",
        "--to",
        "mind",
        "--subject",
        "fix auth",
        "--done-when",
        "it works",
        "--body",
        "it works",
    ]));
    let handle = created
        .lines()
        .find_map(|line| line.strip_prefix("created mind "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no need handle in:\n{created}"));
    ok(&vivi(["pump", "send", "--need", handle, "--project", root]));
}

/// Every archived entry plus every keep-in-place one. Same contents as the
/// library fixture, so a directory rename and the fresh database are covered.
fn seed_home(home: &Path) {
    fs::create_dir_all(home).unwrap();
    for file in [
        "pump.db",
        "pump.db-wal",
        "pump.db-shm",
        "state.json",
        "state.json.imported",
        "reserved.txt",
        "exceptions.txt",
        "status.txt",
        "sync-push.log",
        "log.txt",
    ] {
        fs::write(home.join(file), "seed\n").unwrap();
    }
    for dir in ["plans", "resplit", "targets", "merge-lanes"] {
        fs::create_dir_all(home.join(dir)).unwrap();
    }
    fs::write(home.join("plans").join("need.json"), "{}\n").unwrap();
    fs::write(home.join("log.txt"), "old line\n").unwrap();
}

fn pump_home(project: &tempfile::TempDir) -> PathBuf {
    let home = control::project_home(project.path()).unwrap();
    assert_eq!(home, project.path().join(".vivi").join("pump"));
    home
}

fn snapshot(home: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut rows = Vec::new();
    walk(home, home, &mut rows);
    rows.sort();
    rows
}

fn walk(home: &Path, dir: &Path, rows: &mut Vec<(String, Option<Vec<u8>>)>) {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    for path in paths {
        let rel = path
            .strip_prefix(home)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if path.is_dir() {
            rows.push((rel, None));
            walk(home, &path, rows);
        } else {
            rows.push((rel, Some(fs::read(&path).unwrap())));
        }
    }
}

fn archives(home: &Path) -> Vec<String> {
    fs::read_dir(home.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("pump-archive-"))
        .collect()
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
