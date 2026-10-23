//! `vivi need send --mark`: marks written with the send, validated before it.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

#[test]
fn need_send_mark_shows_in_the_marked_list() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let sent = vivi([
        "need",
        "send",
        "--project",
        root,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "research me",
        "--body",
        "body",
        "--done-when",
        "done",
        "--mark",
        "research",
        "--mark",
        "fix",
    ]);
    assert_success(&sent);

    let listed: Value = serde_json::from_str(&stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--mark",
        "research",
        "--json",
        "--project",
        root,
    ])))
    .unwrap();
    let items = listed.as_array().unwrap();
    assert_eq!(items.len(), 1, "{listed}");
    assert_eq!(items[0]["marks"], serde_json::json!(["fix", "research"]));

    let other = stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--mark",
        "absent",
        "--json",
        "--project",
        root,
    ]));
    let other: Value = serde_json::from_str(&other).unwrap();
    assert!(other.as_array().unwrap().is_empty(), "{other}");
}

#[test]
fn need_send_with_an_invalid_mark_sends_nothing() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let bad = vivi([
        "need",
        "send",
        "--project",
        root,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "never sent",
        "--body",
        "body",
        "--done-when",
        "done",
        "--mark",
        "research",
        "--mark",
        "bad slug",
    ]);
    assert!(!bad.status.success());
    assert!(
        stderr(&bad).contains("invalid mark 'bad slug'"),
        "{}",
        stderr(&bad)
    );

    let listed: Value = serde_json::from_str(&stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--json",
        "--project",
        root,
    ])))
    .unwrap();
    assert!(listed.as_array().unwrap().is_empty(), "{listed}");
}

fn init_roster(project: &Path) {
    let root = project.to_str().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root]));
    for identity in ["ceo", "cto"] {
        assert_success(&vivi([
            "mailspace",
            "identity",
            "add",
            identity,
            "--project",
            root,
        ]));
    }
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
