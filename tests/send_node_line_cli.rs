//! The `node <handle>` receipt line on work sends: a send that mints a
//! backlog node names the handle that holds it, the delivered copy's handle
//! for local recipients, while the pre-existing `created` and `sent` lines
//! keep their exact old format and position.

use std::path::Path;
use std::process::{Command, Output};

const BODY: &str = "it works";

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

fn ok(project: &Path, args: &[&str]) -> String {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--project", project.to_str().unwrap()]);
    let output = vivi(full);
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
    ok(tmp.path(), &["role", "add", "mind"]);
    ok(tmp.path(), &["role", "add", "hand"]);
    tmp
}

fn last_token(line: &str) -> &str {
    line.split_whitespace().last().unwrap()
}

fn line_with_prefix<'a>(stdout: &'a str, prefix: &str) -> &'a str {
    stdout
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no {prefix:?} line in:\n{stdout}"))
}

/// The node handle named on stdout must be the delivered copy's handle, must
/// differ from the sent copy's handle, and must be a node in the backlog
/// graph while the sent handle is not.
fn assert_node_matches_created(
    project: &Path,
    stdout: &str,
    created_line: &str,
    sent_line: &str,
    node_line: &str,
) {
    let created_handle = last_token(created_line);
    let sent_handle = last_token(sent_line);
    let node_handle = last_token(node_line);
    assert_eq!(node_handle, created_handle, "stdout:\n{stdout}");
    assert_ne!(node_handle, sent_handle, "stdout:\n{stdout}");
    let show = ok(project, &["graph", "show", "backlog"]);
    assert!(
        show.lines()
            .any(|line| line.starts_with(&format!("  {node_handle}[\""))),
        "node {node_handle} missing from backlog graph:\n{show}"
    );
    assert!(
        !show
            .lines()
            .any(|line| line.starts_with(&format!("  {sent_handle}[\""))),
        "sent handle {sent_handle} must not be a backlog node:\n{show}"
    );
}

#[test]
fn task_send_prints_the_node_handle_after_the_old_lines() {
    let tmp = project();
    let p = tmp.path();
    let stdout = ok(
        p,
        &[
            "task",
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--subject",
            "Fix the thing",
            "--done-when",
            BODY,
            "--body",
            BODY,
        ],
    );
    // Byte-exact shape: the two old lines first, unchanged, then the node line.
    let created_line = line_with_prefix(&stdout, "created hand ");
    let sent_line = line_with_prefix(&stdout, "sent ");
    let node_line = line_with_prefix(&stdout, "node ");
    assert_eq!(
        stdout,
        format!("{created_line}\n{sent_line}\n{node_line}\n"),
        "unexpected extra or reordered lines"
    );
    assert_eq!(created_line.split_whitespace().count(), 3, "{created_line}");
    assert_eq!(sent_line.split_whitespace().count(), 2, "{sent_line}");
    assert_eq!(node_line.split_whitespace().count(), 2, "{node_line}");
    assert_node_matches_created(p, &stdout, created_line, sent_line, node_line);
    // The node is dispatchable: graph ready lists it as ready work.
    let ready = ok(p, &["graph", "ready"]);
    assert!(
        ready
            .lines()
            .any(|line| line.trim_start().starts_with("ready ")
                && line.contains(last_token(node_line))),
        "node missing from graph ready:\n{ready}"
    );
}

#[test]
fn need_and_want_send_print_the_node_line_too() {
    let tmp = project();
    let p = tmp.path();
    for (kind, flag) in [("need", "--done-when"), ("want", "--question")] {
        let stdout = ok(
            p,
            &[
                kind,
                "send",
                "--from",
                "mind",
                "--to",
                "hand",
                "--subject",
                format!("{kind} item").as_str(),
                flag,
                BODY,
                "--body",
                BODY,
            ],
        );
        let created_line = line_with_prefix(&stdout, "created hand ");
        let sent_line = line_with_prefix(&stdout, "sent ");
        let node_line = line_with_prefix(&stdout, "node ");
        assert_eq!(
            stdout,
            format!("{created_line}\n{sent_line}\n{node_line}\n"),
            "{kind}: unexpected extra or reordered lines"
        );
        assert_node_matches_created(p, &stdout, created_line, sent_line, node_line);
    }
}

#[test]
fn multi_recipient_send_prints_one_node_line_per_delivered_copy() {
    let tmp = project();
    let p = tmp.path();
    let stdout = ok(
        p,
        &[
            "task",
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--cc",
            "mind",
            "--subject",
            "Two holders",
            "--done-when",
            BODY,
            "--body",
            BODY,
        ],
    );
    let created: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("created "))
        .collect();
    let nodes: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("node "))
        .collect();
    let sent_line = line_with_prefix(&stdout, "sent ");
    assert_eq!(created.len(), 2, "stdout:\n{stdout}");
    assert_eq!(nodes.len(), 2, "stdout:\n{stdout}");
    let created_handles: Vec<&str> = created.iter().map(|line| last_token(line)).collect();
    let node_handles: Vec<&str> = nodes.iter().map(|line| last_token(line)).collect();
    assert_eq!(created_handles, node_handles, "stdout:\n{stdout}");
    let sent_handle = last_token(sent_line);
    assert!(
        !node_handles.contains(&sent_handle),
        "sent handle must not be named as a node: {stdout}"
    );
}

#[test]
fn task_from_prints_the_node_line_for_the_lowered_task() {
    let tmp = project();
    let p = tmp.path();
    let want_stdout = ok(
        p,
        &[
            "want",
            "send",
            "--from",
            "mind",
            "--to",
            "mind",
            "--subject",
            "Root want",
            "--question",
            "what to do",
            "--body",
            "a want body",
        ],
    );
    let want_handle = last_token(line_with_prefix(&want_stdout, "created mind "));
    let stdout = ok(
        p,
        &[
            "task",
            "from",
            want_handle,
            "--for",
            "mind",
            "--to",
            "hand",
            "--subject",
            "Task from want",
            "--body",
            BODY,
        ],
    );
    let created_line = line_with_prefix(&stdout, "created hand ");
    let source_line = line_with_prefix(&stdout, "source want ");
    let sent_line = line_with_prefix(&stdout, "sent ");
    let node_line = line_with_prefix(&stdout, "node ");
    assert_eq!(
        stdout,
        format!("{created_line}\n{source_line}\n{sent_line}\n{node_line}\n"),
        "unexpected extra or reordered lines"
    );
    assert_node_matches_created(p, &stdout, created_line, sent_line, node_line);
}

#[test]
fn remote_only_send_holds_the_node_on_the_sent_copy() {
    let tmp = project();
    let p = tmp.path();
    let stdout = ok(
        p,
        &[
            "task",
            "send",
            "--from",
            "mind",
            "--to",
            "hand@3f9a1c7e.local",
            "--subject",
            "Away task",
            "--done-when",
            BODY,
            "--body",
            BODY,
        ],
    );
    // No local recipient: no delivered copy, so the sent copy holds the node.
    assert!(
        !stdout.contains("created "),
        "remote-only send must not print a created line: {stdout}"
    );
    let sent_line = line_with_prefix(&stdout, "sent ");
    let node_line = line_with_prefix(&stdout, "node ");
    assert_eq!(last_token(node_line), last_token(sent_line), "{stdout}");
    let show = ok(p, &["graph", "show", "backlog"]);
    assert!(
        show.lines()
            .any(|line| line.starts_with(&format!("  {}[\"", last_token(node_line)))),
        "sent-copy node missing from backlog graph:\n{show}"
    );
}

#[test]
fn mail_send_prints_no_node_line() {
    let tmp = project();
    let p = tmp.path();
    let stdout = ok(
        p,
        &[
            "mail",
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--subject",
            "Plain mail",
            "--body",
            "hello",
        ],
    );
    assert!(
        !stdout.contains("node "),
        "mail send mints no node and must print no node line: {stdout}"
    );
}
