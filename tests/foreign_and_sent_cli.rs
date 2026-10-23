//! End-to-end tests for the sender's own copy of work addressed to another
//! mailspace: it is tracked, listed and orderable, but never dispatched.

use std::path::Path;
use std::process::{Command, Output};

const REMOTE: &str = "hand@3f9a1c7e.local";
const BODY: &str = "done_when: it works";

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

fn fails(project: &Path, args: &[&str]) -> String {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--project", project.to_str().unwrap()]);
    let output = vivi(full);
    assert!(!output.status.success(), "{args:?} should fail");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    ok(tmp.path(), &["mailspace", "init"]);
    ok(tmp.path(), &["role", "add", "mind"]);
    ok(tmp.path(), &["role", "add", "hand", "--kind", "hand"]);
    tmp
}

fn sent_handle(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("sent "))
        .unwrap()
        .trim()
        .to_string()
}

fn send(project: &Path, kind: &str, to: &str, subject: &str, extra: &[&str]) -> String {
    let mut args = vec![
        kind,
        "send",
        "--from",
        "mind",
        "--to",
        to,
        "--subject",
        subject,
        "--body",
        BODY,
    ];
    args.extend_from_slice(extra);
    sent_handle(&ok(project, &args))
}

#[test]
fn a_sent_copy_is_awaiting_listed_and_orderable_but_never_dispatched() {
    let tmp = project();
    let p = tmp.path();
    let need = send(p, "need", REMOTE, "away need", &[]);
    let task = send(p, "task", REMOTE, "away task", &[]);
    let want = send(p, "want", REMOTE, "away want", &[]);
    let local = ok(
        p,
        &[
            "task",
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--subject",
            "home",
            "--body",
            BODY,
        ],
    );
    assert!(local.contains("created hand"), "{local}");

    let step = ok(p, &["step"]);
    for handle in [&need, &task, &want] {
        let line = step
            .lines()
            .find(|line| line.contains(handle.as_str()))
            .unwrap_or_else(|| panic!("{handle} missing from step:\n{step}"));
        assert!(line.contains("awaiting_remote"), "{line}");
        assert!(line.contains(REMOTE), "{line}");
    }
    assert!(
        step.lines()
            .any(|l| l.contains("home") && l.contains("task"))
    );

    let ready = ok(p, &["graph", "ready"]);
    assert!(ready.contains("ready=1"), "{ready}");
    assert!(ready.contains("awaiting=3"), "{ready}");
    let awaiting = ready.lines().find(|l| l.contains("awaiting ")).unwrap();
    assert!(
        awaiting.contains(&need) && awaiting.contains(&want),
        "{ready}"
    );

    let listed = ok(p, &["need", "list", "--for", "mind", "--status", "open"]);
    assert!(
        listed.contains(&need) && listed.contains("sent"),
        "{listed}"
    );
    let none = ok(p, &["need", "list", "--for", "mind", "--status", "done"]);
    assert!(!none.contains(&need), "{none}");
    let wants = ok(p, &["want", "list", "--for", "mind"]);
    assert!(wants.contains(&want), "{wants}");
    let dump = ok(p, &["task", "dump", "--for", "mind", "--status", "open"]);
    assert!(
        dump.contains(&task) && dump.contains("Role: sent"),
        "{dump}"
    );
    assert!(!ok(p, &["boot"]).contains(&need), "boot lists no sent copy");

    let activate = fails(p, &["graph", "activate", &task, "--task", &task]);
    assert!(activate.contains("handed to"), "{activate}");
    assert!(activate.contains(REMOTE), "{activate}");

    // Orderable: depend on a sent copy; the dependent waits for it.
    let after = send(p, "task", "hand", "after", &["--depends-on", &need]);
    assert!(!after.is_empty());
    let blocked = ok(p, &["graph", "ready"]);
    assert!(
        blocked
            .lines()
            .any(|l| l.contains("blocked ") && !l.contains("(none)")),
        "{blocked}"
    );

    let reopen = fails(p, &["need", "reopen", &need, "--for", "mind"]);
    assert!(reopen.contains("only the receiving mailspace"), "{reopen}");
    let promote = fails(p, &["want", "promote", &want, "--for", "mind"]);
    assert!(
        promote.contains("only the receiving mailspace"),
        "{promote}"
    );

    ok(p, &["need", "done", &need, "--for", "mind"]);
    let cleared = ok(p, &["graph", "ready"]);
    assert!(cleared.contains("awaiting=2"), "{cleared}");
    assert!(cleared.contains("blocked=0"), "{cleared}");
}
