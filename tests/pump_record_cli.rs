//! `vivi pump send|list|show|done|reopen|history` through the real binary.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

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

fn roster(project: &Path) -> String {
    let root = project.to_str().unwrap().to_string();
    ok(&vivi(["mailspace", "init", "--project", &root]));
    // `operator` ships with the mailspace; adding it again is refused.
    for identity in ["mind"] {
        ok(&vivi([
            "mailspace",
            "identity",
            "add",
            identity,
            "--project",
            &root,
        ]));
    }
    root
}

fn send_need(root: &str) -> String {
    let out = ok(&vivi([
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
        "--body",
        "done_when: it works",
    ]));
    out.lines()
        .find_map(|line| line.strip_prefix("created mind "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no need handle in:\n{out}"))
        .to_string()
}

fn records(root: &str, extra: &[&str]) -> Vec<Value> {
    let mut args = vec!["pump", "list", "--json", "--project", root];
    args.extend_from_slice(extra);
    serde_json::from_str::<Value>(&ok(&vivi(args)))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn send_record(root: &str, need: &str) -> String {
    let out = ok(&vivi(["pump", "send", "--need", need, "--project", root]));
    out.split_whitespace().nth(2).unwrap().to_string()
}

#[test]
fn pump_record_lifecycle_through_the_cli() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    let need = send_need(&root);

    let first = send_record(&root, &need);
    let open = records(&root, &[]);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["handle"], first.as_str());
    assert_eq!(open[0]["kind"], "pump");
    assert_eq!(open[0]["status"], "open");
    assert_eq!(open[0]["need"], need.as_str());

    let refused = vivi(["pump", "send", "--need", &need, "--project", &root]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains(&first), "{}", stderr(&refused));

    let shown = ok(&vivi(["pump", "show", &first, "--project", &root]));
    assert!(shown.contains(&format!("need     {need}")), "{shown}");
    assert!(shown.contains("status   open"), "{shown}");

    let reopen_open = vivi(["pump", "reopen", &first, "--project", &root]);
    assert!(!reopen_open.status.success());

    ok(&vivi([
        "pump",
        "done",
        &first,
        "--note",
        "gave-up",
        "--project",
        &root,
    ]));
    assert!(records(&root, &[]).is_empty());
    let done = records(&root, &["--status", "done"]);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["outcome"], "gave-up");

    let second = send_record(&root, &need);
    assert_ne!(first, second);
    let reopen_blocked = vivi(["pump", "reopen", &first, "--project", &root]);
    assert!(!reopen_blocked.status.success());
    assert!(
        stderr(&reopen_blocked).contains(&second),
        "{}",
        stderr(&reopen_blocked)
    );

    let history = ok(&vivi([
        "pump",
        "history",
        &need,
        "--json",
        "--project",
        &root,
    ]));
    let history: Value = serde_json::from_str(&history).unwrap();
    assert_eq!(history[0]["handle"], first.as_str());
    assert_eq!(history[0]["status"], "done");
    assert_eq!(history[1]["handle"], second.as_str());
    assert_eq!(history[1]["status"], "open");
    let all = records(&root, &["--status", "all", "--need", &need]);
    assert_eq!(all.len(), 2);
}

#[test]
fn pump_send_refuses_an_unknown_need() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    let refused = vivi(["pump", "send", "--need", "deadbeef", "--project", &root]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("unknown need"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn pump_records_stay_out_of_the_backlog_graph() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    let need = send_need(&root);
    let record = send_record(&root, &need);
    ok(&vivi(["pump", "done", &record, "--project", &root]));
    let audit = ok(&vivi(["graph", "audit", "--repair", "--project", &root]));
    assert!(!audit.contains(&record), "{audit}");
    let ready = ok(&vivi(["graph", "ready", "--project", &root]));
    assert!(!ready.contains(&record), "{ready}");
}
