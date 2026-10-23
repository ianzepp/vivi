//! `--latest` on `task|need|want show` reads only the newest message of a
//! thread: what an orchestrator wants after `pool wait`, instead of piping
//! the whole thread through sed. The default output must not change.

use std::path::Path;
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::Duration;

const ROOT_BODY: &str = "ROOT-BODY-MARKER-7f3a91cd";
/// Bodies keep the composer's CRLF line endings, so assertions match on the
/// unique single-line markers, never across a line break.
const ROOT_MARKER: &str = "ROOT-BODY-MARKER-7f3a91cd";
const FIRST_REPLY: &str = "FIRST-REPLY-MARKER-b42de801";
const SECOND_REPLY: &str = "SECOND-REPLY-MARKER-09cfee57";
const NEED_BODY: &str = "NEED-BODY-MARKER-53a7d2c0";
const NEED_MARKER: &str = "NEED-BODY-MARKER-53a7d2c0";
const NEED_REPLY: &str = "NEED-REPLY-MARKER-e841ba6f";
const WANT_BODY: &str = "WANT-BODY-MARKER-17d9c4a2";
const WANT_REPLY: &str = "WANT-REPLY-MARKER-6b0f39de";

/// Message Date headers carry second precision; the thread orders by date
/// first, so the sleeps make "newest" deterministic.
fn pause() {
    sleep(Duration::from_millis(1100));
}

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
    for role in ["mind", "hand"] {
        ok(tmp.path(), &["role", "add", role, "--kind", role]);
    }
    tmp
}

/// The handle of the recipient's copy of a send.
fn send(p: &Path, kind: &str, subject: &str, body: &str) -> String {
    // The flag each kind needs for its front matter.
    let (flag, value) = match kind {
        "need" => ("--done-when", "units land"),
        "want" => ("--question", "what lands"),
        _ => ("--done-when", "it works"),
    };
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
            flag,
            value,
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

#[test]
fn latest_prints_only_the_newest_reply_of_three() {
    let tmp = project();
    let p = tmp.path();
    let task = send(p, "task", "T the brief", ROOT_BODY);
    pause();
    reply(p, &task, FIRST_REPLY);
    pause();
    reply(p, &task, SECOND_REPLY);

    // The default output is unchanged: header, all three messages, root last
    // section ordering root -> first -> second (the order --latest reads).
    let default = ok(p, &["task", "show", &task]);
    assert!(default.contains("Thread (3 message(s))"), "{default}");
    let root_at = default
        .find(ROOT_MARKER)
        .unwrap_or_else(|| panic!("{default}"));
    let first_at = default
        .find(FIRST_REPLY)
        .unwrap_or_else(|| panic!("{default}"));
    let second_at = default
        .find(SECOND_REPLY)
        .unwrap_or_else(|| panic!("{default}"));
    assert!(root_at < first_at && first_at < second_at, "{default}");

    // --latest is the newest reply alone: none of the brief, none of the
    // first reply, no thread header.
    let latest = ok(p, &["task", "show", &task, "--latest"]);
    assert!(latest.contains(SECOND_REPLY), "{latest}");
    assert!(!latest.contains(ROOT_MARKER), "{latest}");
    assert!(!latest.contains("it works"), "{latest}");
    assert!(!latest.contains(FIRST_REPLY), "{latest}");
    assert!(!latest.contains("Thread ("), "{latest}");
    assert!(latest.contains("## "), "{latest}");
    assert!(latest.contains("Subject: "), "{latest}");

    // --latest --json keeps the default JSON shape, limited to one message.
    let json = ok(p, &["task", "show", &task, "--latest", "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&json).unwrap();
    let messages = thread.as_array().unwrap();
    assert_eq!(messages.len(), 1, "{json}");
    assert!(
        messages[0]["body"].as_str().unwrap().contains(SECOND_REPLY),
        "{json}"
    );
}

#[test]
fn latest_on_a_root_only_thread_reports_no_replies() {
    let tmp = project();
    let p = tmp.path();
    let task = send(p, "task", "T untouched", ROOT_BODY);
    let out = ok(p, &["task", "show", &task, "--latest"]);
    assert_eq!(out.trim(), "No replies yet (thread has 1 message)", "{out}");
}

#[test]
fn latest_works_on_needs_and_wants() {
    let tmp = project();
    let p = tmp.path();

    // A need with a bound unit: --latest shows the newest message only, so
    // the units block stays out of it while the default show keeps it.
    let need = send(p, "need", "N the need", NEED_BODY);
    let unit = send(p, "task", "T the unit", ROOT_BODY);
    ok(p, &["need", "bind", &need, &unit]);
    pause();
    reply(p, &need, NEED_REPLY);
    let default = ok(p, &["need", "show", &need]);
    assert!(default.contains("units    "), "{default}");
    let latest = ok(p, &["need", "show", &need, "--latest"]);
    assert!(latest.contains(NEED_REPLY), "{latest}");
    assert!(!latest.contains(NEED_MARKER), "{latest}");
    assert!(!latest.contains("units    "), "{latest}");

    // A want with one reply: --latest is the reply, not the want body.
    let want = send(p, "want", "W the want", WANT_BODY);
    pause();
    reply(p, &want, WANT_REPLY);
    let latest = ok(p, &["want", "show", &want, "--latest"]);
    assert!(latest.contains(WANT_REPLY), "{latest}");
    assert!(!latest.contains(WANT_BODY), "{latest}");
}
