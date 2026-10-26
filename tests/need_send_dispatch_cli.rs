//! `vivi need send --dispatch` through the real binary: the shared fixtures
//! plus the two base behaviors — an unmarked dispatch opens exactly one pump
//! record with the ordinary `pump send` defaults, and a plain send stays
//! unchanged.
//!
//! The child modules carry the follow-on coverage (dispatch identity,
//! dispatch refusals, dispatch priority) and reach the helpers below through
//! `super::*`.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

#[path = "support/need_send_dispatch_identity.rs"]
mod identity;
#[path = "support/need_send_dispatch_priority.rs"]
mod priority;
#[path = "support/need_send_dispatch_refusal.rs"]
mod refusal;

#[test]
fn need_send_dispatch_opens_one_record() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());

    // The starter `[pump]` table stays deliberately incomplete: validate is
    // "not ready" (exit 3). Opening a record must not need pools or execute
    // work, so the dispatch still goes through on that config.
    let validate = vivi(["config", "validate", "--project", &root]);
    assert_eq!(
        validate.status.code(),
        Some(3),
        "expected the starter pump to be not ready (exit 3)\n{}",
        stdout(&validate)
    );
    assert!(
        stdout(&validate).contains("not ready"),
        "{}",
        stdout(&validate)
    );

    let out = ok(&vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "mind",
        "--subject",
        "fix auth",
        "--done-when",
        "it works",
        "--body",
        "please fix login",
        "--dispatch",
    ]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "unexpected receipts:\n{out}");
    let delivered = handle_after(&out, "created mind ");
    let sent = handle_after(&out, "sent ");
    assert_eq!(lines[0], format!("created mind {delivered}"));
    assert_eq!(lines[1], format!("sent {sent}"));
    assert_eq!(lines[2], format!("node {delivered}"));
    let record = handle_after(&out, "pump record ");
    assert_eq!(
        lines[3],
        format!("pump record {record} for need {delivered}")
    );

    // Exactly one open record, and it references the filed need's content.
    let listed = records(&root);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["handle"], record.as_str());
    assert_eq!(listed[0]["kind"], "pump");
    assert_eq!(listed[0]["status"], "open");
    assert_eq!(listed[0]["need"], delivered.as_str());

    // The record keeps the ordinary `pump send` defaults — the configured
    // `[pump] from` and `[pump] reply_from` — not the need sender.
    let from = listed[0]["from"].as_str().unwrap();
    let to = listed[0]["to"].as_str().unwrap();
    assert!(from.starts_with("operator@"), "{from}");
    assert!(to.starts_with("mind@"), "{to}");
    assert_eq!(
        listed[0]["subject"].as_str().unwrap(),
        format!("pump {delivered}: fix auth")
    );

    // The stored letter carries the default dispatch prose, never the need
    // sender's body.
    let shown: Value = serde_json::from_str(&ok(&vivi([
        "pump",
        "show",
        &record,
        "--json",
        "--project",
        &root,
    ])))
    .unwrap();
    assert_eq!(shown["from"], listed[0]["from"]);
    assert_eq!(shown["to"], listed[0]["to"]);
    let body = shown["body"].as_str().unwrap();
    assert!(body.contains(&format!("need = \"{delivered}\"")), "{body}");
    assert!(
        body.contains(&format!("Dispatch of need {delivered}.")),
        "{body}"
    );
    assert!(!body.contains("please fix login"), "{body}");

    // The filed need itself is untouched by the dispatch.
    let need_shown = ok(&vivi(["need", "show", &delivered, "--project", &root]));
    assert!(need_shown.contains("From: filer@"), "{need_shown}");
    assert!(need_shown.contains("please fix login"), "{need_shown}");
}

#[test]
fn plain_need_send_is_unchanged() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());

    let out = ok(&vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "mind",
        "--subject",
        "fix auth",
        "--done-when",
        "it works",
        "--target",
        "vivi",
        "--write-scope",
        "src/a.rs",
        "--body",
        "please fix login",
        "--mark",
        "research",
        "--mark",
        "fix",
    ]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 7, "unexpected receipts:\n{out}");
    let delivered = handle_after(&out, "created mind ");
    let sent = handle_after(&out, "sent ");
    assert_eq!(lines[0], format!("created mind {delivered}"));
    assert_eq!(lines[1], format!("sent {sent}"));
    assert_eq!(lines[2], format!("node {delivered}"));
    // Marks travel on the sender's own copy first, then the delivered copy.
    assert_eq!(lines[3], format!("marked {sent} research"));
    assert_eq!(lines[4], format!("marked {sent} fix"));
    assert_eq!(lines[5], format!("marked {delivered} research"));
    assert_eq!(lines[6], format!("marked {delivered} fix"));
    assert!(!out.contains("pump record"), "{out}");

    // Front matter sugar and marks land on the delivered copy.
    let shown = ok(&vivi(["need", "show", &delivered, "--project", &root]));
    assert!(shown.contains("done_when = [\"it works\"]"), "{shown}");
    assert!(shown.contains("target = \"vivi\""), "{shown}");
    assert!(shown.contains("write_scope = [\"src/a.rs\"]"), "{shown}");
    assert!(shown.contains("please fix login"), "{shown}");
    assert!(shown.contains("Marks: fix research"), "{shown}");
    let listed = listed_needs(&root, &["--mark", "research"]);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["handle"], delivered.as_str());
    assert_eq!(listed[0]["marks"], serde_json::json!(["fix", "research"]));

    // No record, and no priority database. The database check must come
    // before any priority-list operation: with a `[pump]` table present the
    // list itself would create `pump.db`.
    assert!(records(&root).is_empty(), "a plain send opened a record");
    let db = project.path().join(".vivi").join("pump").join("pump.db");
    assert!(!db.exists(), "{:?} exists", db);

    // With the starter `[pump]` table gone the effective configuration has
    // no pump: validate reports nothing about the pump, and a priority list
    // is refused instead of creating the database.
    remove_starter_pump(project.path());
    let validate = ok(&vivi(["config", "validate", "--project", &root]));
    assert!(!validate.contains("not ready"), "{validate}");
    assert!(!validate.contains("pump."), "{validate}");
    let refused = vivi(["pump", "priority", "list", "--project", &root]);
    assert!(
        !refused.status.success(),
        "priority list without a pump table succeeded"
    );
    assert!(
        stderr(&refused).contains("no [pump] table in .vivi/mailspace.toml"),
        "{}",
        stderr(&refused)
    );
    assert!(!db.exists(), "{:?} exists", db);
    assert!(records(&root).is_empty());

    // A plain send still files the need and still opens no record.
    let again = ok(&vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "mind",
        "--subject",
        "fix auth again",
        "--done-when",
        "it works twice",
        "--body",
        "please fix login again",
    ]));
    let again_delivered = handle_after(&again, "created mind ");
    assert_ne!(again_delivered, delivered);
    assert!(!again.contains("pump record"), "{again}");
    assert!(records(&root).is_empty());
    assert!(!db.exists(), "{:?} exists", db);
}

/// Run `vivi` with `args` and return the raw output.
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

/// The decoded stdout of an output.
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The decoded stderr of an output.
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Assert the command succeeded and return its stdout.
fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
    stdout(output)
}

/// The first whitespace token after `prefix` on the first line that carries
/// it: the handle a receipt names.
fn handle_after(output: &str, prefix: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing '{prefix}' in output:\n{output}"))
        .to_string()
}

/// Every pump record of the mailspace, as `pump list --json` reports them.
fn records(root: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(&ok(&vivi(["pump", "list", "--json", "--project", root])))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// The needs `mind` holds, as `need list --json` reports them, with `extra`
/// filters added.
fn listed_needs(root: &str, extra: &[&str]) -> Vec<Value> {
    let mut args = vec!["need", "list", "--for", "mind", "--json", "--project", root];
    args.extend_from_slice(extra);
    serde_json::from_str::<Value>(&ok(&vivi(args)))
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// Initialize the mailspace and add the identities the dispatch tests use.
///
/// `operator` ships with `mailspace init` and is the default `[pump] from`.
/// `mind` is the default `[pump] reply_from`, so it must be a recipient of
/// every dispatched need. `hand` is the default `[pump] to`; it joins as a
/// full hand-kind seat, because a thin identity has kind none and that turns
/// `config validate` into a hard error. `filer` sends the needs, so the
/// record sender stays distinct from the need sender.
fn roster(project: &Path) -> String {
    let root = project.to_str().unwrap().to_string();
    ok(&vivi(["mailspace", "init", "--project", &root]));
    ok(&vivi([
        "mailspace",
        "identity",
        "add",
        "mind",
        "--project",
        &root,
    ]));
    ok(&vivi([
        "role",
        "add",
        "hand",
        "--kind",
        "hand",
        "--project",
        &root,
    ]));
    ok(&vivi([
        "mailspace",
        "identity",
        "add",
        "filer",
        "--project",
        &root,
    ]));
    root
}

/// Remove the starter `[pump]` table `mailspace init` appended, leaving the
/// effective configuration with no pump at all.
fn remove_starter_pump(project: &Path) {
    let config = project.join(".vivi").join("mailspace.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let marker = "# Starter pump configuration.";
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("no starter [pump] block in {}", config.display()));
    std::fs::write(&config, &text[..start]).unwrap();
}
