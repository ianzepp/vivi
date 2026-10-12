//! A hand's `done` is a report, not an acceptance: it moves the hand's own
//! row and nothing else. The item's author or a mind accepts, and only the
//! accept completes the node, unlocks dependents and fires a bound need's
//! join. A mailspace with no `[sync]` carries all of it.

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

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    ok(tmp.path(), &["mailspace", "init"]);
    for (role, kind) in [
        ("mind", "mind"),
        ("hand", "hand"),
        ("auditor", "auditor"),
        ("operator", "operator"),
    ] {
        ok(tmp.path(), &["role", "add", role, "--kind", kind]);
    }
    tmp
}

/// The handle the hand's delivered copy of a send has.
fn send(p: &Path, kind: &str, subject: &str, depends_on: &[&str]) -> String {
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
    for dep in depends_on {
        args.extend(["--depends-on", dep]);
    }
    let out = ok(p, &args);
    out.lines()
        .find_map(|line| line.strip_prefix("created hand "))
        .unwrap()
        .trim()
        .to_string()
}

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

fn line_of(text: &str, label: &str) -> String {
    text.lines()
        .find(|line| line.trim_start().starts_with(label))
        .unwrap_or_else(|| panic!("no '{label}' line in:\n{text}"))
        .trim()
        .to_string()
}

#[test]
fn a_report_completes_nothing_until_the_mind_accepts_it() {
    let tmp = project();
    let p = tmp.path();
    let need = send(p, "need", "N the need", &[]);
    let first = send(p, "task", "T1 first unit", &[]);
    let second = send(p, "task", "T2 second unit", &[&first]);
    ok(p, &["need", "bind", &need, &first, &second]);

    // The hand reports T1.
    let reported = ok(p, &["task", "done", &first, "--for", "hand"]);
    assert!(reported.contains("; reported"), "{reported}");
    assert_eq!(role_of(p, "task", &first), "done", "its own row moved");
    assert_eq!(state_line(p, "task", &first), None, "no event written");

    // The frontier names it, keeps T2 blocked, and the need open.
    let ready = ok(p, &["graph", "ready", "backlog"]);
    assert!(line_of(&ready, "counts").contains("reported=1"), "{ready}");
    assert!(line_of(&ready, "reported").contains(&first), "{ready}");
    assert!(line_of(&ready, "blocked").contains(&second), "{ready}");
    assert!(!line_of(&ready, "ready").contains(&second), "{ready}");
    assert_eq!(role_of(p, "need", &need), "needs");
    assert_eq!(state_line(p, "need", &need), None, "no need event yet");

    // Step lists it as awaiting acceptance and never as a dispatch.
    let step = ok(p, &["step"]);
    let exception = step
        .lines()
        .find(|line| line.contains("awaiting_acceptance"))
        .unwrap_or_else(|| panic!("{step}"));
    assert!(exception.contains("awaiting_acceptance"), "{step}");
    assert!(exception.contains("reported done by hand"), "{step}");
    assert!(
        exception.contains(&format!("vivi task done {first} --for mind")),
        "{step}"
    );
    assert!(step.contains("no dispatches"), "{step}");

    // Apply does not accept it either.
    let applied = ok(p, &["step", "--apply", &first]);
    assert!(applied.contains("awaiting_acceptance"), "{applied}");
    assert!(applied.contains("no decisions"), "{applied}");
    assert!(!line_of(&ok(p, &["graph", "ready", "backlog"]), "reported").contains("(none)"));

    // The audit calls it a report, not drift, and a repair leaves it.
    let audit = ok(p, &["graph", "audit", "--repair"]);
    assert!(audit.contains("state=0"), "{audit}");
    assert!(!audit.contains("[repaired]"), "{audit}");
    assert!(
        line_of(&ok(p, &["graph", "ready", "backlog"]), "reported").contains(&first),
        "the repair left the report alone"
    );

    // The mind accepts: the event, the node, the dependent, nothing more.
    let accepted = ok(p, &["task", "done", &first, "--for", "mind"]);
    assert!(accepted.contains("event"), "{accepted}");
    assert_eq!(state_line(p, "task", &first).as_deref(), Some("done"));
    let ready = ok(p, &["graph", "ready", "backlog"]);
    assert!(line_of(&ready, "counts").contains("reported=0"), "{ready}");
    assert!(line_of(&ready, "ready").contains(&second), "{ready}");
    assert_eq!(role_of(p, "need", &need), "needs", "T2 is still open");

    // T2 reported and accepted: the join closes the need with its event.
    ok(p, &["task", "done", &second, "--for", "hand"]);
    assert_eq!(role_of(p, "need", &need), "needs");
    ok(p, &["task", "done", &second, "--for", "mind"]);
    assert_eq!(role_of(p, "need", &need), "done");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    assert!(ok(p, &["step"]).contains("no dispatches"));
    let audit = ok(p, &["graph", "audit", "--repair"]);
    assert!(
        audit.contains("missing=0 state=0 kind=0 orphan=0 foreign=0 edge=0 event=0"),
        "{audit}"
    );
}

#[test]
fn a_hand_that_reported_in_error_reopens_its_own_report() {
    let tmp = project();
    let p = tmp.path();
    let task = send(p, "task", "T the unit", &[]);
    ok(p, &["task", "done", &task, "--for", "hand"]);
    assert!(
        line_of(&ok(p, &["graph", "ready", "backlog"]), "reported").contains(&task),
        "reported"
    );

    ok(p, &["task", "reopen", &task, "--for", "hand"]);

    assert_eq!(role_of(p, "task", &task), "tasks");
    let ready = ok(p, &["graph", "ready", "backlog"]);
    assert!(line_of(&ready, "reported").contains("(none)"), "{ready}");
    assert!(line_of(&ready, "ready").contains(&task), "{ready}");
}

#[test]
fn activating_a_reported_node_is_refused_with_the_accept_command() {
    let tmp = project();
    let p = tmp.path();
    let task = send(p, "task", "T the unit", &[]);
    ok(p, &["task", "done", &task, "--for", "hand"]);

    let out = vivi(
        p,
        &[
            "graph",
            "activate",
            &format!("backlog:{task}"),
            "--task",
            &task,
        ],
    );

    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("awaits acceptance"), "{err}");
    assert!(
        err.contains(&format!("vivi task done {task} --for mind")),
        "{err}"
    );
}
