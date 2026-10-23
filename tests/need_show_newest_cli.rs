//! `need show --newest N` prints the newest N messages of a thread, oldest
//! first, instead of the default first-50-in-walk-order cap.

use std::path::Path;
use std::process::{Command, Output};

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

/// The handle of the delivered copy of a need under the recipient.
fn send_need(p: &Path, subject: &str) -> String {
    let out = ok(
        p,
        &[
            "need",
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--subject",
            subject,
            "--body",
            "need body",
            "--done-when",
            "done",
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

fn bodies(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("REPLY-"))
        .map(str::to_string)
        .collect()
}

/// Same-second messages tie-break on content id, so a small wait keeps the
/// date order (and therefore "newest") deterministic.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
}

#[test]
fn newest_selects_the_last_n_replies_in_order() {
    let tmp = project();
    let p = tmp.path();
    let need = send_need(p, "N the threaded need");
    for i in 0..3 {
        reply(p, &need, &format!("REPLY-{i} body {i}"));
        settle();
    }

    let out = ok(p, &["need", "show", &need, "--newest", "2"]);
    assert_eq!(
        bodies(&out),
        vec!["REPLY-1 body 1", "REPLY-2 body 2"],
        "{out}"
    );
    assert!(!out.contains("REPLY-0"), "{out}");

    // Default output still shows the whole (short) thread, oldest first.
    let all = ok(p, &["need", "show", &need]);
    assert_eq!(
        bodies(&all),
        (0..3)
            .map(|i| format!("REPLY-{i} body {i}"))
            .collect::<Vec<_>>(),
        "{all}"
    );
}

#[test]
fn newest_larger_than_the_thread_shows_everything() {
    let tmp = project();
    let p = tmp.path();
    let need = send_need(p, "N the small need");
    reply(p, &need, "REPLY-only body");

    let out = ok(p, &["need", "show", &need, "--newest", "50"]);
    assert_eq!(bodies(&out), vec!["REPLY-only body"], "{out}");
}

#[test]
fn newest_with_json_keeps_the_default_shape() {
    let tmp = project();
    let p = tmp.path();
    let need = send_need(p, "N the json need");
    reply(p, &need, "REPLY-a body");
    settle();
    reply(p, &need, "REPLY-b body");

    let out = ok(p, &["need", "show", &need, "--newest", "1", "--json"]);
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed.len(), 1, "{out}");
    assert!(parsed[0]["body"].as_str().unwrap().contains("REPLY-b"));
}
