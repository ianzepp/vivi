//! State as letters in a mailspace with no `[sync]`: the owner's close and
//! reopen are reply letters on the item, an assignee's report is only a
//! reply, and the folders, nodes, thread and frontier follow the letters.

use std::path::Path;
use std::process::{Command, Output};

const BODY: &str = "done_when: it works";

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

fn fails(project: &Path, args: &[&str]) -> String {
    let output = vivi(project, args);
    assert!(!output.status.success(), "{args:?} should fail");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    ok(tmp.path(), &["mailspace", "init"]);
    for role in ["mind", "hand", "auditor"] {
        ok(tmp.path(), &["role", "add", role]);
    }
    tmp
}

/// `(the delivered copy's handle, the sender's copy's handle)`.
fn send(p: &Path, kind: &str, subject: &str) -> (String, String) {
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
            BODY,
        ],
    );
    let after = |prefix: &str| {
        out.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap()
            .trim()
            .to_string()
    };
    (after("created hand "), after("sent "))
}

/// The folder role of the row `handle` names, from `show`'s JSON thread.
fn role_of(p: &Path, kind: &str, handle: &str) -> String {
    let out = ok(p, &[kind, "show", handle, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&out).unwrap();
    thread
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["handle"] == handle)
        .unwrap()["role"]
        .as_str()
        .unwrap()
        .to_string()
}

fn state_line(p: &Path, kind: &str, handle: &str) -> Option<String> {
    ok(p, &[kind, "show", handle])
        .lines()
        .find_map(|line| line.strip_prefix("State: "))
        .map(|rest| rest.split(' ').next().unwrap().to_string())
}

fn thread_len(p: &Path, kind: &str, handle: &str) -> usize {
    let out = ok(p, &[kind, "show", handle, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&out).unwrap();
    thread.as_array().unwrap().len()
}

fn counts(p: &Path) -> String {
    ok(p, &["graph", "ready", "backlog"])
        .lines()
        .find(|line| line.trim_start().starts_with("counts"))
        .unwrap()
        .trim()
        .to_string()
}

fn audit_is_clean(p: &Path) {
    let audit = ok(p, &["graph", "audit"]);
    assert!(
        audit.contains("missing=0 state=0 kind=0 orphan=0 foreign=0 edge=0 event=0"),
        "{audit}"
    );
    let repair = ok(p, &["graph", "audit", "--repair"]);
    assert!(!repair.contains("[repaired]"), "{repair}");
}

#[test]
fn the_owners_flow_with_no_sync_is_letters_all_the_way() {
    let tmp = project();
    let p = tmp.path();
    let (need, need_sent) = send(p, "need", "N the need");
    let (task, task_sent) = send(p, "task", "T the unit");
    ok(p, &["need", "bind", &need, &task]);
    assert!(counts(p).contains("ready=2"), "{}", counts(p));
    assert_eq!(role_of(p, "task", &task), "tasks");
    assert_eq!(state_line(p, "task", &task), None);

    // The assignee reports with a reply. Nothing of the owner's moves.
    ok(
        p,
        &[
            "mail",
            "reply",
            &task,
            "--from",
            "hand",
            "--body",
            "T is done: it works",
        ],
    );
    assert_eq!(role_of(p, "task", &task), "tasks");
    assert_eq!(
        state_line(p, "task", &task),
        None,
        "a report is not an event"
    );
    assert!(counts(p).contains("ready=2"), "{}", counts(p));
    assert_eq!(thread_len(p, "task", &task), 2);

    // The owner closes T: an event letter, the unit's row, the node, and
    // through the join the need.
    ok(p, &["task", "done", &task_sent, "--for", "mind"]);
    assert_eq!(role_of(p, "task", &task), "done");
    assert_eq!(state_line(p, "task", &task).as_deref(), Some("done"));
    assert_eq!(thread_len(p, "task", &task), 3);
    assert_eq!(role_of(p, "need", &need), "done", "the join closed N");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    assert!(counts(p).contains("ready=0"), "{}", counts(p));
    let step = ok(p, &["step"]);
    assert!(step.contains("no dispatches"), "{step}");
    audit_is_clean(p);

    // An auditor asks for a reopen; the owner reopens N with a note.
    ok(
        p,
        &[
            "mail",
            "reply",
            &need,
            "--from",
            "auditor",
            "--body",
            "please reopen N",
        ],
    );
    assert_eq!(role_of(p, "need", &need), "done", "a request moves nothing");
    let refused = fails(p, &["need", "reopen", &need, "--for", "hand"]);
    assert!(refused.contains("only the owner can reopen"), "{refused}");
    ok(
        p,
        &[
            "need",
            "reopen",
            &need_sent,
            "--for",
            "mind",
            "--note",
            "reopening on the audit",
        ],
    );
    assert_eq!(role_of(p, "need", &need), "needs");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("reopened"));
    assert!(counts(p).contains("ready=1"), "{}", counts(p));
    let ready = ok(p, &["graph", "ready", "backlog"]);
    assert!(ready.contains(&need), "{ready}");

    // A second unit, reported and closed; the join closes N again.
    let (task2, task2_sent) = send(p, "task", "T2 the second unit");
    ok(p, &["need", "bind", &need, &task2]);
    assert!(ready_has(p, &task2));
    ok(
        p,
        &[
            "mail",
            "reply",
            &task2,
            "--from",
            "hand",
            "--body",
            "T2 is done",
        ],
    );
    ok(p, &["task", "done", &task2_sent, "--for", "mind"]);
    assert_eq!(role_of(p, "task", &task2), "done");
    assert_eq!(role_of(p, "need", &need), "done");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    assert!(counts(p).contains("ready=0"), "{}", counts(p));

    // The need's thread is its letters: the item, the audit's request, and
    // the events in order.
    let out = ok(p, &["need", "show", &need, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&out).unwrap();
    let bodies: Vec<String> = thread
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap().trim().to_string())
        .collect();
    assert_eq!(bodies.len(), 5, "{bodies:?}");
    for wanted in ["please reopen N", "reopening on the audit"] {
        assert!(bodies.iter().any(|b| b == wanted), "{wanted}: {bodies:?}");
    }
    assert_eq!(bodies.iter().filter(|b| *b == "done").count(), 2);

    // Converged: an audit and a repair change nothing.
    audit_is_clean(p);
    let again = ok(p, &["need", "show", &need, "--json"]);
    assert_eq!(again, out);
}

fn ready_has(p: &Path, handle: &str) -> bool {
    ok(p, &["graph", "ready", "backlog"]).contains(handle)
}

#[test]
fn an_assignees_done_moves_its_own_copy_and_writes_no_event() {
    let tmp = project();
    let p = tmp.path();
    let (task, _) = send(p, "task", "T the unit");

    ok(
        p,
        &["task", "done", &task, "--for", "hand", "--note", "finished"],
    );

    assert_eq!(role_of(p, "task", &task), "done");
    assert_eq!(state_line(p, "task", &task), None, "a report, not an event");
    assert_eq!(thread_len(p, "task", &task), 2, "the item and the note");
    audit_is_clean(p);
}

#[test]
fn a_self_addressed_need_closes_and_reopens_by_its_own_letters() {
    let tmp = project();
    let p = tmp.path();
    let out = ok(
        p,
        &[
            "need",
            "send",
            "--from",
            "mind",
            "--to",
            "mind",
            "--subject",
            "mine",
            "--body",
            BODY,
        ],
    );
    let need = out
        .lines()
        .find_map(|l| l.strip_prefix("created mind "))
        .unwrap()
        .trim()
        .to_string();

    ok(
        p,
        &[
            "need",
            "done",
            &need,
            "--for",
            "mind",
            "--note",
            "finished it",
        ],
    );
    assert_eq!(role_of(p, "need", &need), "done");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    let status = ok(p, &["mailspace", "status"]);
    assert!(status.contains("total unread mail: 0"), "{status}");

    ok(p, &["need", "reopen", &need, "--for", "mind"]);
    assert_eq!(role_of(p, "need", &need), "needs");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("reopened"));
    audit_is_clean(p);
}
