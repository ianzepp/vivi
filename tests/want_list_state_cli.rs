//! `want list` status and `--status open|done` follow the computed state.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

#[test]
fn want_list_follows_serving_needs() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let want = send(root, "want", "the want", &["--question", "q"]);
    let need = send(
        root,
        "need",
        "serving need",
        &["--done-when", "a", "--serves", &want],
    );
    assert_eq!(statuses(root, "open", &want), ["open"]);
    assert!(statuses(root, "done", &want).is_empty());

    vivi_ok(&["need", "done", &need, "--for", "cto", "--project", root]);
    assert!(statuses(root, "open", &want).is_empty());
    assert_eq!(statuses(root, "done", &want), ["done"]);
    assert_eq!(statuses(root, "all", &want), ["done"]);
}

#[test]
fn hand_closed_want_with_a_new_serving_need_lists_open() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let want = send(root, "want", "closed by hand", &["--question", "q"]);
    vivi_ok(&["want", "done", &want, "--for", "cto", "--project", root]);
    assert_eq!(statuses(root, "done", &want), ["done"]);
    assert!(statuses(root, "open", &want).is_empty());

    send(
        root,
        "need",
        "late need",
        &["--done-when", "a", "--serves", &want],
    );
    assert_eq!(statuses(root, "open", &want), ["open"]);
    assert!(statuses(root, "done", &want).is_empty());
    assert_eq!(statuses(root, "all", &want), ["open"]);
}

/// The JSON `status` of each listed row for `want` under `--status`.
fn statuses(root: &str, status: &str, want: &str) -> Vec<String> {
    let out = stdout(&vivi_ok(&[
        "want",
        "list",
        "--status",
        status,
        "--for",
        "cto",
        "--json",
        "--project",
        root,
    ]));
    let rows: Value = serde_json::from_str(&out).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .filter(|row| row["handle"] == want)
        .map(|row| row["status"].as_str().unwrap().to_string())
        .collect()
}

fn send(root: &str, kind: &str, subject: &str, extra: &[&str]) -> String {
    let mut args = vec![
        kind,
        "send",
        "--project",
        root,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        subject,
        "--body",
        "prose body",
    ];
    args.extend_from_slice(extra);
    let out = stdout(&vivi_ok(&args));
    out.lines()
        .find_map(|line| line.strip_prefix("created cto"))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing 'created cto' in output:\n{out}"))
        .to_string()
}

fn init_roster(project: &Path) {
    let root = project.to_str().unwrap();
    vivi_ok(&["mailspace", "init", "--project", root]);
    for identity in ["ceo", "cto"] {
        vivi_ok(&["mailspace", "identity", "add", identity, "--project", root]);
    }
}

fn vivi_ok(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status: {}\nargs: {args:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
