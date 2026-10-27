//! A role reply collected through the production receipt path: the pool result
//! text the pump replies with, read back by `vivi task show <task> --latest`,
//! must give the hand message (no CR) that the slicer envelope parser accepts.

use std::fs;
use std::process::{Command, Output};

use vivi::pump::attempt_record::hand_message;
use vivi::pump::envelope::roles::SlicerAnswer;
use vivi::pump::flow::fanout::parse_reply;

const ENVELOPE: &str = "+++\n\
answer = \"sliced\"\n\
\n\
[[slice]]\n\
id = \"A1\"\n\
title = 'first slice'\n\
brief = '''\n\
Open the hand brief.\n\
\n\
--- Begin document ---\n\
\n\
Embedded context for the hand.\n\
\n\
--- End document ---\n\
\n\
Keep this closing note in the brief.\n\
'''\n\
target = \"vivi\"\n\
write_scope = [\"src/a.rs\"]\n\
deps = []\n\
+++\n\
notes for the pump\n";

/// The exact brief the multiline literal in [`ENVELOPE`] carries: blank lines
/// before both lookalike headings and real content after the end heading.
const BRIEF: &str = "Open the hand brief.\n\n--- Begin document ---\n\nEmbedded context for the hand.\n\n--- End document ---\n\nKeep this closing note in the brief.\n";

/// The genuine trailing pool sections written after the envelope, outside it:
/// the receipt must be cut before them, never folded around them.
const TAILS: &str = "\n\n--- stderr tail\nthread 'hand' panicked at src/hand.rs:1:1:\nboom\n\n--- runner log tail\n10:02:03 INFO attempt 1 model m\n10:02:09 INFO hand exit 0\n";

#[test]
fn a_slicer_reply_survives_the_task_show_receipt_path() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root]));
    // `mailspace init` seeds the operator role; the task goes to a mind.
    assert_success(&vivi([
        "role",
        "add",
        "mind",
        "--kind",
        "mind",
        "--project",
        root,
    ]));

    let task_body = project.path().join("task.txt");
    fs::write(&task_body, "slice this need\n").unwrap();
    let sent = vivi([
        "task",
        "send",
        "--from",
        "operator",
        "--to",
        "mind",
        "--subject",
        "slice step",
        "--body-file",
        task_body.to_str().unwrap(),
        "--project",
        root,
    ]);
    assert_success(&sent);
    let task = stdout(&sent)
        .lines()
        .find_map(|line| line.strip_prefix("created mind ").map(str::to_owned))
        .unwrap_or_else(|| panic!("no handle in {}", stdout(&sent)));

    // Same-second messages tie-break on content id; a wait keeps the reply
    // the newest message of the thread.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let result = format!(
        "pool result job-1: finished\npool local  slot 1  model m\nfrom mind  vivi\nexit 0\n\n--- receipt\n{}{TAILS}",
        ENVELOPE.trim_end()
    );
    let reply_body = project.path().join("reply.txt");
    fs::write(&reply_body, &result).unwrap();
    assert_success(&vivi([
        "mail",
        "reply",
        &task,
        "--from",
        "mind",
        "--body-file",
        reply_body.to_str().unwrap(),
        "--project",
        root,
    ]));

    let shown = vivi(["task", "show", &task, "--latest", "--project", root]);
    assert_success(&shown);
    let out = stdout(&shown);
    assert!(!out.contains('\r'), "task show output carries CR: {out:?}");
    let message = hand_message(&out);
    // The trailing pool sections stay outside the hand message.
    assert!(
        !message.contains("--- stderr tail"),
        "stderr leaked: {message:?}"
    );
    assert!(!message.contains("panicked"), "stderr leaked: {message:?}");
    assert!(
        !message.contains("--- runner log tail"),
        "runner log leaked: {message:?}"
    );
    assert!(
        !message.contains("INFO attempt"),
        "runner log leaked: {message:?}"
    );
    let slicer = parse_reply(&message).unwrap_or_else(|err| {
        panic!("envelope refused: {err:?}\nmessage: {message:?}\nout: {out:?}")
    });
    assert_eq!(slicer.answer, SlicerAnswer::Sliced, "answer: {slicer:?}");
    assert_eq!(slicer.slice.len(), 1, "one slice: {slicer:?}");
    let slice = &slicer.slice[0];
    assert_eq!(slice.id, "A1");
    assert_eq!(slice.title, "first slice");
    assert_eq!(slice.target, "vivi");
    assert_eq!(slice.write_scope, vec!["src/a.rs".to_owned()]);
    assert!(slice.deps.is_empty(), "deps: {:?}", slice.deps);
    assert_eq!(slice.brief, BRIEF, "brief: {:?}", slice.brief);
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
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
