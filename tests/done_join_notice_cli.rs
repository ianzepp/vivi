//! A `done` that completes a bound need through its join says so in one
//! line: `need <handle> completed (all bound units accepted)`. A `done` that
//! settles nothing — a worker's report, an acceptance with bound units still
//! open, work bound to no need — prints no such line.

use std::path::Path;
use std::process::{Command, Output};

const BODY: &str = "the unit of work";
const NOTICE: &str = "completed (all bound units accepted)";

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

/// The handle the hand's delivered copy of a send has.
fn send(p: &Path, kind: &str, subject: &str) -> String {
    let mut args = vec![
        kind,
        "send",
        "--from",
        "mind",
        "--to",
        "hand",
        "--subject",
        subject,
        "--body",
        BODY,
    ];
    if kind == "need" {
        args.extend(["--done-when", "it works"]);
    }
    let out = ok(p, &args);
    out.lines()
        .find_map(|line| line.strip_prefix("created hand "))
        .unwrap()
        .trim()
        .to_string()
}

/// The join-notice lines a command output carries.
fn notice_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|line| line.contains(NOTICE)).collect()
}

fn state_line(p: &Path, kind: &str, handle: &str) -> Option<String> {
    ok(p, &[kind, "show", handle])
        .lines()
        .find_map(|line| line.strip_prefix("State: "))
        .map(|rest| rest.split(' ').next().unwrap().to_string())
}

#[test]
fn the_accept_that_closes_a_bound_need_prints_one_notice_line() {
    let tmp = project();
    let p = tmp.path();
    let need = send(p, "need", "N the need");
    let first = send(p, "task", "T1 first unit");
    let second = send(p, "task", "T2 second unit");
    ok(p, &["need", "bind", &need, &first, &second]);

    // The first acceptance leaves the second unit open: no notice.
    let first_done = ok(p, &["task", "done", &first, "--for", "mind"]);
    assert!(first_done.contains("event"), "{first_done}");
    assert!(notice_lines(&first_done).is_empty(), "{first_done}");

    // The second acceptance completes the join: exactly one notice line
    // naming the need handle as the mailspace shows it.
    let second_done = ok(p, &["task", "done", &second, "--for", "mind"]);
    assert_eq!(
        notice_lines(&second_done),
        vec![format!("need {need} {NOTICE}").as_str()],
        "{second_done}"
    );

    // `need show` agrees the need is done.
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
}

#[test]
fn a_done_on_a_task_bound_to_no_need_prints_no_notice() {
    let tmp = project();
    let p = tmp.path();
    let task = send(p, "task", "T the loose task");

    let done = ok(p, &["task", "done", &task, "--for", "mind"]);

    assert!(done.contains("event"), "{done}");
    assert!(notice_lines(&done).is_empty(), "{done}");
}

#[test]
fn a_workers_report_prints_no_notice() {
    let tmp = project();
    let p = tmp.path();
    let need = send(p, "need", "N the need");
    let unit = send(p, "task", "T the only unit");
    ok(p, &["need", "bind", &need, &unit]);

    // The hand's done is a report, not an acceptance: it settles nothing,
    // fires no join, and so prints no notice.
    let report = ok(p, &["task", "done", &unit, "--for", "hand"]);

    assert!(report.contains("; reported"), "{report}");
    assert!(notice_lines(&report).is_empty(), "{report}");
    assert_eq!(state_line(p, "need", &need), None, "the need stays open");
}
