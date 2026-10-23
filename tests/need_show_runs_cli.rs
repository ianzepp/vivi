//! `need show` prints `runs: N` for a need with N bound run sentinels, and the
//! board's need line carries `rN`; a plain need shows neither.

use std::path::Path;
use std::process::{Command, Output};

fn vivi(project: &Path, args: &[&str]) -> Output {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--project", project.to_str().unwrap()]);
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(full)
        .output()
        .unwrap()
}

fn ok(project: &Path, args: &[&str]) -> String {
    let output = vivi(project, args);
    assert!(
        output.status.success(),
        "{args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    ok(tmp.path(), &["mailspace", "init"]);
    for (role, kind) in [("mind", "mind"), ("hand", "hand")] {
        ok(tmp.path(), &["role", "add", role, "--kind", kind]);
    }
    tmp
}

/// The handle of the hand's delivered copy of a send.
fn send(p: &Path, kind: &str, subject: &str, body: &str) -> String {
    let out = ok(
        p,
        &[
            kind,
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--subject",
            subject,
            "--body",
            body,
        ],
    );
    out.lines()
        .find_map(|line| line.strip_prefix("created hand "))
        .unwrap()
        .trim()
        .to_string()
}

fn board_line(board: &str, handle: &str) -> String {
    board
        .lines()
        .find(|line| line.contains(handle))
        .unwrap_or_else(|| panic!("no board line for {handle}\n{board}"))
        .to_string()
}

#[test]
fn a_need_with_two_bound_sentinels_shows_two_runs() {
    let tmp = project();
    let p = tmp.path();
    let need = send(p, "need", "N flow need", "need body");
    let first = send(
        p,
        "task",
        "sentinel 1",
        &format!("pump-run-sentinel: {need} r1"),
    );
    let second = send(
        p,
        "task",
        "sentinel 2",
        &format!("pump-run-sentinel: {need} r2"),
    );
    let plain = send(p, "task", "plain unit", "ordinary unit body");
    ok(p, &["need", "bind", &need, &first, &second, &plain]);

    let shown = ok(p, &["need", "show", &need]);
    assert!(shown.lines().any(|l| l.trim() == "runs: 2"), "{shown}");

    let board = ok(p, &["board"]);
    assert!(board_line(&board, &need).ends_with("  r2"), "{board}");
}

#[test]
fn a_plain_need_shows_no_runs() {
    let tmp = project();
    let p = tmp.path();
    let need = send(p, "need", "N plain need", "need body");
    let unit = send(p, "task", "plain unit", "ordinary unit body");
    ok(p, &["need", "bind", &need, &unit]);

    let shown = ok(p, &["need", "show", &need]);
    assert!(!shown.contains("runs:"), "{shown}");

    let board = ok(p, &["board"]);
    let line = board_line(&board, &need);
    assert!(!line.contains("  r0") && !line.ends_with("r1"), "{line}");
    assert!(line.ends_with("N plain need"), "{line}");
}
