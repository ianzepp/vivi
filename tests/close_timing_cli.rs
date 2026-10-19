//! Closing a need or a task prints a `timing:` summary line built from the
//! attempt data on that item's thread: `need done` reads the pump's attempt
//! records, `task done` reads the pool's result replies.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

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

/// The handle of the delivered copy of a send under the recipient.
fn send(p: &Path, kind: &str, subject: &str, body: &str) -> String {
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
            body,
        ],
    );
    out.lines()
        .find_map(|line| line.strip_prefix("created hand "))
        .unwrap()
        .trim()
        .to_string()
}

fn reply(p: &Path, handle: &str, body: &str) {
    ok(
        p,
        &["mail", "reply", handle, "--from", "hand", "--body", body],
    );
}

/// Same-second messages tie-break on content id, so a small wait keeps the
/// date order (and therefore the anchor-first order) deterministic.
fn settle() {
    std::thread::sleep(Duration::from_millis(1100));
}

/// An attempt record body as the pump writes it: the twelve-line block, then
/// the prose tail that must not be read as fields.
fn record(attempt: &str, agent_seconds: u64) -> String {
    format!(
        "pump: attempt record\n\
         attempt: {attempt}\n\
         need: 8f2c1d40\n\
         task: c366c7b7\n\
         actor: aeff60b0/burgus\n\
         role: hand\n\
         pool: here\n\
         slot: s1\n\
         model: zai/glm-5.3\n\
         start: 999940\n\
         end: 1000000\n\
         agent-seconds: {agent_seconds}\n\
         outcome: delivered\n\
         branch: unit/{attempt} (pushed to the target remote, kept)\n\
         base: origin/next\n"
    )
}

#[test]
fn need_done_prints_the_timing_summary() {
    let tmp = project();
    let p = tmp.path();
    let need = send(p, "need", "N the timed need", "done_when: it works");
    settle();
    reply(p, &need, &record("n1-1-a1", 3600));

    let done = ok(p, &["need", "done", &need, "--for", "hand"]);

    // The open-to-done duration is wall clock, so only the line and the
    // attempt arithmetic are pinned.
    assert!(done.contains("timing: open-to-done "), "{done}");
    assert!(
        done.contains("attempts 1; agent time 1h; slowest n1-1-a1 (1h)"),
        "{done}"
    );
}

#[test]
fn task_done_prints_the_timing_summary() {
    let tmp = project();
    let p = tmp.path();
    let task = send(p, "task", "s", "done_when: x");
    settle();
    reply(
        p,
        &task,
        "pool result j123: finished\n\
         pool fly4  slot s1  model zai/glm\n\
         from next  default\n\
         push: pushed unit/n1-1-a1 abcdef0123\n\
         agent exit 0 in 12s, job 20s, started from 1700000000\n",
    );

    let done = ok(p, &["task", "done", &task, "--for", "hand"]);

    assert!(done.contains("timing: open-to-done "), "{done}");
    assert!(
        done.contains("attempts 1; agent time 12s; slowest j123 (12s)"),
        "{done}"
    );
}
