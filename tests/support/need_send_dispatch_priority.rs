//! Dispatch priority coverage lands here: `--priority` is written only
//! after the record is minted, keyed by the record's own need (never the
//! sent handle or the record handle), level 0 keeps the zero-clearing
//! semantics of `vivi pump priority set`, a dispatch without `--priority`
//! never creates `pump.db`, and a failed priority write leaves the filed
//! need and the open record untouched.
//!
//! The tests in this module reach the fixtures of the parent integration
//! test (`roster`, `vivi`, `ok`, `records`, `handle_after`, ...) through
//! `super::*`.

use std::path::{Path, PathBuf};

use super::*;
use vivi::pump::flow::rank::need_key;
use vivi::pump::store::{DB_FILENAME, PumpStore};

/// The non-default pump home the tests configure, relative to the project
/// root, so the tests can tell the configured home from the default one.
const PUMP_HOME: &str = "pump-home";

#[test]
fn dispatch_priority_uses_canonical_need_and_configured_home() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    use_pump_home(project.path());

    let out = ok(&vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "mind",
        "--to",
        "hand",
        "--subject",
        "fix auth",
        "--done-when",
        "it works",
        "--body",
        "please fix login",
        "--dispatch",
        "--priority",
        "7",
    ]));
    let (record, need) = record_and_need(&out);

    // The record receipt names the canonical need, and the record listing
    // agrees: the stored key is the record's own need, not a sent or copy
    // handle picked from the receipts.
    let listed = records(&root);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["handle"], record.as_str());
    assert_eq!(listed[0]["need"], need.as_str());

    // Exactly one row, keyed by the canonical need, in the configured home —
    // read back read-only before any listing API could create a database.
    let db = home(&root).join(DB_FILENAME);
    assert!(db.is_file(), "{:?} is missing", db);
    let store = PumpStore::open_read_only(&db)
        .unwrap()
        .expect("configured-home pump.db exists");
    let rows = store.need_priorities().unwrap();
    assert_eq!(rows, vec![(need.clone(), 7)]);

    // The public priority list and its CLI see the same sole row.
    assert_eq!(
        vivi::pump::priority::list(&home(&root)).unwrap(),
        vec![(need.clone(), 7)]
    );
    assert_eq!(
        ok(&vivi(["pump", "priority", "list", "--project", &root])),
        format!("{need} 7\n")
    );

    // The default home stayed unused.
    assert!(!default_home_db(project.path()).exists());

    // The persisted level ranks the need ahead of an otherwise comparable
    // zero-priority candidate: same class, same age, higher level wins
    // whatever the tie-breaking handles are.
    let urgent = need_key(false, rows[0].1, 0, &need);
    let plain = need_key(false, 0, 0, "a");
    assert!(urgent < plain, "{urgent:?} must rank before {plain:?}");

    // The filed need carries no manufactured dispatch or priority field.
    let shown = ok(&vivi(["need", "show", &need, "--project", &root]));
    assert!(shown.contains("done_when = [\"it works\"]"), "{shown}");
    assert!(shown.contains("please fix login"), "{shown}");
    assert!(!shown.contains("dispatch"), "{shown}");
    assert!(!shown.contains("priority"), "{shown}");
}

#[test]
fn dispatch_zero_and_omitted_priority_have_no_row() {
    // Explicit zero: a successful dispatch that stores no row — level 0
    // keeps the clearing semantics of `vivi pump priority set`, so the
    // database exists but holds nothing.
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    use_pump_home(project.path());
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
        "--priority",
        "0",
    ]));
    let (record, need) = record_and_need(&out);
    assert_eq!(records(&root)[0]["handle"], record.as_str());
    assert_eq!(records(&root)[0]["need"], need.as_str());
    let db = home(&root).join(DB_FILENAME);
    let store = PumpStore::open_read_only(&db)
        .unwrap()
        .expect("a zero dispatch still creates pump.db");
    assert!(
        store.need_priorities().unwrap().is_empty(),
        "level 0 stored a row"
    );
    assert!(vivi::pump::priority::list(&home(&root)).unwrap().is_empty());
    assert_eq!(
        ok(&vivi(["pump", "priority", "list", "--project", &root])),
        ""
    );

    // Omission: no write at all, so the configured home never gains a
    // database. The absence checks come before any list API, whose open
    // would create the file.
    let other = tempfile::tempdir().unwrap();
    let other_root = roster(other.path());
    use_pump_home(other.path());
    let out = ok(&vivi([
        "need",
        "send",
        "--project",
        &other_root,
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
    assert!(out.contains("pump record"), "{out}");
    let db = home(&other_root).join(DB_FILENAME);
    assert!(!db.exists(), "{:?} exists", db);
    assert!(
        PumpStore::open_read_only(&db).unwrap().is_none(),
        "an omitted priority created a database"
    );
    assert_eq!(
        ok(&vivi([
            "pump",
            "priority",
            "list",
            "--project",
            &other_root
        ])),
        ""
    );
    assert!(
        vivi::pump::priority::list(&home(&other_root))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn priority_write_failure_preserves_filing_and_record() {
    let project = tempfile::tempdir().unwrap();
    let root = roster(project.path());
    use_pump_home(project.path());

    // A directory at the database path makes every priority write fail
    // deterministically, without relying on permissions.
    std::fs::create_dir_all(home(&root)).unwrap();
    std::fs::create_dir(home(&root).join(DB_FILENAME)).unwrap();

    let failed = vivi([
        "need",
        "send",
        "--project",
        &root,
        "--from",
        "filer",
        "--to",
        "mind",
        "--to",
        "hand",
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
        !failed.status.success(),
        "the broken priority write must fail the dispatch\n{}",
        stdout(&failed)
    );
    let out = stdout(&failed);
    let error = stderr(&failed);
    let sent = handle_after(&out, "sent ");
    let (record, need) = record_and_need(&out);

    // The failure names the dispatch boundary with the sent handle, the
    // priority-write phase with the canonical need and the minted record,
    // and the underlying database issue.
    assert!(
        error.contains(&format!(
            "dispatch of filed need {sent} failed: priority for dispatched need {need} \
             (pump record {record}) failed"
        )),
        "{error}"
    );
    assert!(error.contains("pump.db open"), "{error}");

    // The ordinary need receipts and the pump receipt came first, in order:
    // the record was minted and reported before the priority write failed.
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("created "), "{out}");
    assert!(
        lines.contains(&format!("sent {sent}").as_str()),
        "missing sent receipt:\n{out}"
    );
    assert_eq!(
        lines.last().copied(),
        Some(format!("pump record {record} for need {need}").as_str())
    );

    // Reopened mailspace: exactly the reported open record survives.
    let listed = records(&root);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0]["handle"], record.as_str());
    assert_eq!(listed[0]["status"], "open");
    assert_eq!(listed[0]["need"], need.as_str());

    // The filed need keeps its body and front matter.
    let shown = ok(&vivi(["need", "show", &need, "--project", &root]));
    assert!(shown.contains("please fix login"), "{shown}");
    assert!(shown.contains("done_when = [\"it works\"]"), "{shown}");

    // The marks the send wrote are still stored on the delivered copy.
    let marked = listed_needs(&root, &["--mark", "research"]);
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert_eq!(marked[0]["handle"], handle_after(&out, "created mind "));
    assert_eq!(marked[0]["marks"], serde_json::json!(["research"]));

    // The backlog node is intact: ready in the frontier, no audit drift.
    let ready = ok(&vivi(["graph", "ready", "--project", &root]));
    assert!(ready.contains(&need), "{ready}");
    let audit = ok(&vivi(["graph", "audit", "--project", &root]));
    assert!(audit.contains("findings missing=0"), "{audit}");
}

/// The configured pump home of a project root, holding `pump.db`.
fn home(root: &str) -> PathBuf {
    Path::new(root).join(PUMP_HOME)
}

/// The pump database of the default home, which the configured-home tests
/// must leave unused.
fn default_home_db(project: &Path) -> PathBuf {
    project.join(".vivi").join("pump").join(DB_FILENAME)
}

/// Point the starter `[pump]` table at the non-default [`PUMP_HOME`],
/// relative to the project root.
fn use_pump_home(project: &Path) {
    let config = project.join(".vivi").join("mailspace.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let marker = "[pump]\n";
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("no [pump] table in {}", config.display()));
    let end = start + marker.len();
    std::fs::write(
        &config,
        format!("{}home = \"{PUMP_HOME}\"\n{}", &text[..end], &text[end..]),
    )
    .unwrap();
}

/// The record handle and the canonical need key of the `pump record {handle}
/// for need {need}` receipt: the record's own need, never a sent or copy
/// handle read off the other receipts.
fn record_and_need(out: &str) -> (String, String) {
    let line = out
        .lines()
        .find(|line| line.starts_with("pump record "))
        .unwrap_or_else(|| panic!("no pump record receipt in:\n{out}"));
    let (handle, need) = line
        .split_once(" for need ")
        .unwrap_or_else(|| panic!("malformed pump record receipt: {line}"));
    (
        handle.strip_prefix("pump record ").unwrap().to_string(),
        need.trim().to_string(),
    )
}
