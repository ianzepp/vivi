//! Dispatch refusals through the real binary: a `--dispatch` send that the
//! pump cannot take fails only after the need is filed, wraps the failure in
//! the dispatch boundary wording (naming the filed `sent` handle), and never
//! leaves a pump record or a priority write behind.
//!
//! Covered here:
//!
//! - a dispatch where no delivered copy addresses the pump (`mind`, the
//!   starter `[pump] reply_from` default): the only local delivery goes to a
//!   different role, and the same-name pump address in another mailspace
//!   (`mind@<foreign-prefix>.local`) is remote, with no delivered copy;
//! - a dispatch without any `[pump]` table at all: the send files first,
//!   then the missing-pump-configuration refusal fires;
//! - `--priority` argument errors (missing `--dispatch`, invalid levels)
//!   refuse before anything is filed, with no storage side effect.
//!
//! The tests in this module reach the fixtures of the parent integration
//! test (`roster`, `vivi`, `stdout`, `stderr`, `records`, `handle_after`,
//! `listed_needs`, `remove_starter_pump`, ...) through `super::*`.

use super::*;
use std::path::Path;

use serde_json::Value;

/// A dispatch whose pump never receives the need is refused after filing.
///
/// The need is delivered to `hand` (a different local role) with the pump's
/// own name in another mailspace on Cc: `mind@<foreign-prefix>.local` is
/// remote and gets no delivered copy, so no delivered copy addresses the
/// canonical pump identity `mind` (the starter `[pump] reply_from` default).
/// The filing receipts, the stored need, its backlog node and the marks all
/// survive; no record opens and no priority is written.
#[test]
fn dispatch_not_addressed_keeps_the_need() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    let foreign = format!("mind@{}.local", foreign_prefix(&root));

    let out = vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "hand",
        "--cc",
        &foreign,
        "--subject",
        "fix auth",
        "--done-when",
        "it works",
        "--body",
        "please fix login",
        "--mark",
        "research",
        "--dispatch",
        "--priority",
        "5",
    ]);
    assert!(
        !out.status.success(),
        "a dispatch without a delivered copy for the pump must fail"
    );

    // The filing receipts survive: one local delivery (hand), the sent
    // copy, its backlog node, and both mark writes. The foreign address is
    // remote: it got no delivered copy and appears in no `created` line.
    let out_stdout = stdout(&out);
    let delivered = handle_after(&out_stdout, "created hand ");
    let sent = handle_after(&out_stdout, "sent ");
    let lines: Vec<&str> = out_stdout.lines().collect();
    assert_eq!(lines.len(), 5, "unexpected receipts:\n{out_stdout}");
    assert_eq!(lines[0], format!("created hand {delivered}"));
    assert_eq!(lines[1], format!("sent {sent}"));
    assert_eq!(lines[2], format!("node {delivered}"));
    assert_eq!(lines[3], format!("marked {sent} research"));
    assert_eq!(lines[4], format!("marked {delivered} research"));
    assert!(!out_stdout.contains("created mind"), "{out_stdout}");
    assert!(!out_stdout.contains(&foreign), "{out_stdout}");
    assert!(!out_stdout.contains("pump record"), "{out_stdout}");

    // The refusal names the dispatch boundary with the filed handle and the
    // not-addressed issue with the canonical pump identity.
    let err = stderr(&out);
    assert!(
        err.contains(&format!("dispatch of filed need {sent} failed:")),
        "{err}"
    );
    assert!(
        err.contains(
            "no delivered copy of the filed need for mind; the dispatch pump must be a recipient"
        ),
        "{err}"
    );
    // The foreign same-name recipient was classified remote, not local.
    assert!(err.contains(&foreign), "{err}");
    assert!(err.contains("will not leave this checkout"), "{err}");

    // The filed need, its marks and its backlog node are all retained.
    let listed = listed_for(&root, "hand");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["handle"], delivered.as_str());
    assert_eq!(listed[0]["marks"], serde_json::json!(["research"]));
    let ready = ok(&vivi(["graph", "ready", "--project", &root]));
    assert!(
        ready
            .lines()
            .any(|line| line.trim_start().starts_with("ready ") && line.contains(&delivered)),
        "node missing from graph ready:\n{ready}"
    );

    // No record opened and no priority database was written.
    assert!(
        records(&root).is_empty(),
        "a refused dispatch opened a record"
    );
    let db = project.path().join(".vivi").join("pump").join("pump.db");
    assert!(!db.exists(), "{:?} exists", db);
}

/// A dispatch without any `[pump]` table files the need first, then fails
/// with the missing-pump-configuration issue, keeping the need, node and
/// marks and writing neither a record nor a priority.
#[test]
fn dispatch_without_pump_config_keeps_the_need() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    remove_starter_pump(project.path());
    let db = project.path().join(".vivi").join("pump").join("pump.db");

    // The discovered mailspace has no pump: the priority path resolves its
    // home through Mailspace::discover and refuses without a table.
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

    let out = vivi([
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
        "--mark",
        "research",
        "--dispatch",
        "--priority",
        "7",
    ]);
    assert!(
        !out.status.success(),
        "a dispatch without a [pump] table must fail"
    );

    // The need filed before the refusal: full filing receipts on stdout.
    let out_stdout = stdout(&out);
    let delivered = handle_after(&out_stdout, "created mind ");
    let sent = handle_after(&out_stdout, "sent ");
    let lines: Vec<&str> = out_stdout.lines().collect();
    assert_eq!(lines.len(), 5, "unexpected receipts:\n{out_stdout}");
    assert_eq!(lines[0], format!("created mind {delivered}"));
    assert_eq!(lines[1], format!("sent {sent}"));
    assert_eq!(lines[2], format!("node {delivered}"));
    assert_eq!(lines[3], format!("marked {sent} research"));
    assert_eq!(lines[4], format!("marked {delivered} research"));
    assert!(!out_stdout.contains("pump record"), "{out_stdout}");

    // The refusal wraps the missing-pump-configuration issue in the
    // dispatch boundary, naming the filed handle.
    let err = stderr(&out);
    assert!(
        err.contains(&format!("dispatch of filed need {sent} failed:")),
        "{err}"
    );
    assert!(
        err.contains("no [pump] table in .vivi/mailspace.toml; --dispatch needs a configured pump"),
        "{err}"
    );

    // Need, marks and backlog node retained; no record, no priority.
    let listed = listed_needs(&root, &[]);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["handle"], delivered.as_str());
    assert_eq!(listed[0]["marks"], serde_json::json!(["research"]));
    let ready = ok(&vivi(["graph", "ready", "--project", &root]));
    assert!(
        ready
            .lines()
            .any(|line| line.trim_start().starts_with("ready ") && line.contains(&delivered)),
        "node missing from graph ready:\n{ready}"
    );
    assert!(
        records(&root).is_empty(),
        "a refused dispatch opened a record"
    );
    assert!(!db.exists(), "{:?} exists", db);
}

/// `--priority` without `--dispatch` is a clap argument error: it refuses
/// with the parser's error exit before anything is filed, leaving no need,
/// no sent copy, no backlog work, no record and no priority database.
#[test]
fn priority_requires_dispatch() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    for level in ["0", "7"] {
        let flag = format!("--priority={level}");
        let out = vivi([
            "need",
            "send",
            "--project",
            &root,
            "--from",
            "filer",
            "--to",
            "mind",
            "--subject",
            "priority without dispatch",
            "--body",
            "work",
            &flag,
        ]);
        // Clap's usage-error exit, before any filing receipt.
        assert_eq!(
            out.status.code(),
            Some(2),
            "expected a clap argument error for {flag}\n{}",
            stdout(&out)
        );
        assert!(stdout(&out).is_empty(), "{}", stdout(&out));
        let err = stderr(&out);
        assert!(err.contains("error:"), "{err}");
        assert!(err.contains("--dispatch"), "{err}");
        assert_nothing_stored(&root);
    }
}

/// Invalid `--priority` levels are clap value-validation errors: they refuse
/// with the parser's error exit before anything is filed, leaving no need,
/// no sent copy, no backlog work, no record and no priority database.
#[test]
fn priority_rejects_invalid_levels() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    for level in ["-1", "1.5", "99999999999999999999"] {
        let flag = format!("--priority={level}");
        let out = vivi([
            "need",
            "send",
            "--project",
            &root,
            "--from",
            "filer",
            "--to",
            "mind",
            "--subject",
            "invalid priority",
            "--body",
            "work",
            "--dispatch",
            &flag,
        ]);
        // Clap's usage-error exit, before any filing receipt.
        assert_eq!(
            out.status.code(),
            Some(2),
            "expected a clap argument error for {flag}\n{}",
            stdout(&out)
        );
        assert!(stdout(&out).is_empty(), "{}", stdout(&out));
        let err = stderr(&out);
        assert!(err.contains("error:"), "{err}");
        assert!(err.contains("--priority"), "{err}");
        assert_nothing_stored(&root);
    }
}

/// Nothing from a refused send may be stored: no need for the recipient or
/// the sender, no pump record, no priority database, and no work in any
/// backlog frontier.
fn assert_nothing_stored(root: &str) {
    for identity in ["mind", "filer"] {
        let listed = listed_for(root, identity);
        assert!(listed.is_empty(), "{identity} holds {listed:?}");
    }
    assert!(records(root).is_empty(), "a refused send opened a record");
    let db = Path::new(root).join(".vivi").join("pump").join("pump.db");
    assert!(!db.exists(), "{:?} exists", db);
    let ready = ok(&vivi(["graph", "ready", "--json", "--project", root]));
    let frontiers = serde_json::from_str::<Value>(&ready)
        .unwrap_or_else(|error| panic!("unreadable frontier JSON: {error}\n{ready}"));
    let frontiers = frontiers.as_array().unwrap();
    for frontier in frontiers {
        for bucket in [
            "ready", "blocked", "active", "awaiting", "reported", "gates",
        ] {
            let nodes = frontier[bucket]
                .as_array()
                .unwrap_or_else(|| panic!("missing {bucket} in frontier:\n{ready}"));
            assert!(
                nodes.is_empty(),
                "refused send left {bucket} work:\n{ready}"
            );
        }
    }
}

/// The needs `identity` holds, as `need list --json` reports them.
fn listed_for(root: &str, identity: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(&ok(&vivi([
        "need",
        "list",
        "--for",
        identity,
        "--json",
        "--project",
        root,
    ])))
    .unwrap()
    .as_array()
    .unwrap()
    .clone()
}

/// An eight-hex prefix that differs from this mailspace's own: the config's
/// `id` line carries the UUID whose first eight characters are the prefix,
/// and the first character is flipped.
fn foreign_prefix(root: &str) -> String {
    let config =
        std::fs::read_to_string(Path::new(root).join(".vivi").join("mailspace.toml")).unwrap();
    let id = config
        .lines()
        .find_map(|line| line.strip_prefix("id = \""))
        .unwrap_or_else(|| panic!("no id line in {}", config));
    let own = id.get(..8).unwrap_or_else(|| panic!("short id: {config}"));
    let mut foreign = own.to_string();
    foreign.replace_range(..1, if own.starts_with('0') { "1" } else { "0" });
    assert_ne!(foreign, own, "the foreign prefix must differ: {config}");
    foreign
}
