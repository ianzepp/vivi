//! `vivi need|task|want depend`: add and remove prerequisites after the send.

use std::path::Path;
use std::process::{Command, Output};

#[test]
fn need_depend_adds_and_removes_prerequisites_and_graph_follows() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let work = send_work(project.path(), "need", "work", &[]);
    let release = send_work(project.path(), "need", "release", &[&work.held]);

    // The release was sent depending on the work: blocked.
    assert!(blocked(root).contains(&release.held), "{}", blocked(root));
    let show = stdout(&vivi(["need", "show", &release.held, "--project", root]));
    assert!(show.contains(&format!("depends  {}:open", work.held)), "{show}");

    // Remove the header edge: ready.
    let removed = vivi([
        "need",
        "depend",
        &release.sent,
        "--remove",
        &work.held,
        "--for",
        "ceo",
        "--project",
        root,
    ]);
    assert_success(&removed);
    assert!(
        stdout(&removed).contains("no longer depends on"),
        "{}",
        stdout(&removed)
    );
    assert!(!blocked(root).contains(&release.held), "{}", blocked(root));

    // Add a second prerequisite and the first back, several at once.
    let other = send_work(project.path(), "need", "other", &[]);
    let added = vivi([
        "need",
        "depend",
        &release.sent,
        "--add",
        &work.held,
        &other.held,
        "--for",
        "ceo",
        "--project",
        root,
    ]);
    assert_success(&added);
    let text = stdout(&added);
    assert!(text.contains("now depends on"), "{text}");
    assert!(blocked(root).contains(&release.held), "{}", blocked(root));
    let show = stdout(&vivi(["need", "show", &release.held, "--project", root]));
    assert!(show.contains(&work.held) && show.contains(&other.held), "{show}");

    // Again: nothing changes.
    let again = vivi([
        "need",
        "depend",
        &release.sent,
        "--add",
        &work.held,
        "--for",
        "ceo",
        "--project",
        root,
    ]);
    assert_success(&again);
    assert!(stdout(&again).contains("already depends on"), "{}", stdout(&again));
}

#[test]
fn depend_refuses_unknown_handles_other_roles_and_empty_requests() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let work = send_work(project.path(), "need", "work", &[]);
    let release = send_work(project.path(), "need", "release", &[]);

    let unknown = vivi([
        "need",
        "depend",
        &release.sent,
        "--add",
        &work.held,
        "deadbeef",
        "--for",
        "ceo",
        "--project",
        root,
    ]);
    assert!(!unknown.status.success());
    assert!(stderr(&unknown).contains("deadbeef"), "{}", stderr(&unknown));
    assert!(!blocked(root).contains(&release.held), "{}", blocked(root));

    let other_role = vivi([
        "need",
        "depend",
        &release.held,
        "--add",
        &work.held,
        "--for",
        "cto",
        "--project",
        root,
    ]);
    assert!(!other_role.status.success());
    assert!(
        stderr(&other_role).contains("may not change the dependencies"),
        "{}",
        stderr(&other_role)
    );

    let empty = vivi([
        "need",
        "depend",
        &release.sent,
        "--for",
        "ceo",
        "--project",
        root,
    ]);
    assert!(!empty.status.success());
    assert!(stderr(&empty).contains("--add or --remove"), "{}", stderr(&empty));
}

#[test]
fn task_and_want_depend_are_wired() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    for kind in ["task", "want"] {
        let first = send_work(project.path(), kind, "first", &[]);
        let second = send_work(project.path(), kind, "second", &[]);
        let output = vivi([
            kind,
            "depend",
            &second.sent,
            "--add",
            &first.held,
            "--for",
            "ceo",
            "--project",
            root,
        ]);
        assert_success(&output);
        assert!(stdout(&output).contains("now depends on"), "{}", stdout(&output));
    }
}

/// The two handles `send` prints: the recipient's copy (the node) and the
/// sender's own (the copy its author acts through).
struct Sent {
    held: String,
    sent: String,
}

fn send_work(project: &Path, kind: &str, subject: &str, depends_on: &[&String]) -> Sent {
    let mut args: Vec<String> = [
        kind,
        "send",
        "--project",
        project.to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        subject,
        "--body",
        "body",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    for dep in depends_on {
        args.push("--depends-on".into());
        args.push((*dep).clone());
    }
    let output = vivi(args);
    assert_success(&output);
    let text = stdout(&output);
    Sent {
        held: handle_after(&text, "created cto"),
        sent: handle_after(&text, "sent"),
    }
}

fn blocked(root: &str) -> String {
    let output = vivi(["graph", "blocked", "--project", root]);
    assert_success(&output);
    stdout(&output)
}

fn init_roster(project: &Path) {
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.to_str().unwrap(),
    ]));
    for identity in ["ceo", "cto"] {
        assert_success(&vivi([
            "mailspace",
            "identity",
            "add",
            identity,
            "--project",
            project.to_str().unwrap(),
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

fn handle_after(output: &str, prefix: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing '{prefix}' in output:\n{output}"))
        .to_string()
}
