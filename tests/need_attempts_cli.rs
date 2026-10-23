//! `need attempts` reads the pump's structured attempt records off a need
//! thread: every record's fields plus the anchor's open epoch, in one JSON
//! object.

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

/// Same-second messages tie-break on content id, so a small wait keeps the
/// date order (and therefore the attempts order) deterministic.
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
fn attempts_json_lists_every_record_with_the_fields() {
    let tmp = project();
    let p = tmp.path();
    let need = send_need(p, "N the recorded need");
    reply(p, &need, &record("n1-1-a1", 60));
    settle();
    reply(p, &need, &record("n1-1-a2", 3600));

    let out = ok(p, &["need", "attempts", &need, "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

    let attempts = parsed["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "{out}");

    let first = &attempts[0];
    assert_eq!(first["attempt"], "n1-1-a1", "{out}");
    assert_eq!(first["need"], "8f2c1d40", "{out}");
    assert_eq!(first["task"], "c366c7b7", "{out}");
    assert_eq!(first["actor"], "aeff60b0/burgus", "{out}");
    assert_eq!(first["role"], "hand", "{out}");
    assert_eq!(first["pool"], "here", "{out}");
    assert_eq!(first["slot"], "s1", "{out}");
    assert_eq!(first["model"], "zai/glm-5.3", "{out}");
    assert_eq!(first["start"], 999_940, "{out}");
    assert_eq!(first["end"], 1_000_000, "{out}");
    assert_eq!(first["agent_seconds"], 60, "{out}");
    assert_eq!(first["outcome"], "delivered", "{out}");
    assert_eq!(attempts[1]["attempt"], "n1-1-a2", "{out}");
    assert_eq!(attempts[1]["agent_seconds"], 3600, "{out}");
    assert_eq!(parsed["need"], need.as_str(), "{out}");
    assert!(
        parsed["opened"].as_u64().is_some_and(|epoch| epoch > 0),
        "{out}"
    );

    let text = ok(p, &["need", "attempts", &need]);
    assert!(
        text.lines()
            .any(|line| line == "n1-1-a1  delivered  60s  zai/glm-5.3  here/s1"),
        "{text}"
    );
}

#[test]
fn attempts_with_no_records_returns_an_empty_array() {
    let tmp = project();
    let p = tmp.path();
    let need = send_need(p, "N the plain need");
    reply(p, &need, "REPLY-plain body with no record");

    let out = ok(p, &["need", "attempts", &need, "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["attempts"].as_array().unwrap().len(), 0, "{out}");
    assert!(
        parsed["opened"].as_u64().is_some_and(|epoch| epoch > 0),
        "{out}"
    );
}

/// Both copies of a sent need share one date, so thread assembly may place
/// either copy first. `--json` must report the handle the caller named.
#[test]
fn attempts_json_reports_the_named_anchor_for_sent_and_delivered_copies() {
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
            "hand",
            "--subject",
            "N the anchor need",
            "--body",
            "need body",
            "--done-when",
            "done",
        ],
    );
    let delivered = out
        .lines()
        .find_map(|line| line.strip_prefix("created hand "))
        .unwrap()
        .trim()
        .to_string();
    let sent = out
        .lines()
        .find_map(|line| line.strip_prefix("sent "))
        .unwrap()
        .trim()
        .to_string();
    assert_ne!(sent, delivered, "{out}");

    for handle in [&delivered, &sent] {
        let json = ok(p, &["need", "attempts", handle, "--json"]);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["need"], handle.as_str(), "{json}");
        assert!(
            parsed["opened"].as_u64().is_some_and(|epoch| epoch > 0),
            "{json}"
        );
    }
}
