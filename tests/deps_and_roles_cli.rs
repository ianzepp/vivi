//! A mailspace with no `[sync]` at all: letters filed by any route carry
//! their dependency edges, and a letter held for a role the mailspace does
//! not have is visible, never dispatched, and adopted by `role add`.

use std::path::Path;
use std::process::{Command, Output};

use vivi::mailspace::Mailspace;
use vivi::storage::MessageIngestRequest;

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

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    ok(tmp.path(), &["mailspace", "init"]);
    ok(tmp.path(), &["role", "add", "mind"]);
    ok(tmp.path(), &["role", "add", "hand"]);
    tmp
}

fn address(project: &Path, role: &str) -> String {
    let listing = ok(project, &["mailspace", "identity", "list"]);
    listing
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{role} ")))
        .unwrap()
        .trim()
        .to_string()
}

fn deliver(project: &Path, folder: &str, subject: &str, kind: &str, deps: &[&str]) {
    let mut headers = String::new();
    for dep in deps {
        headers.push_str(&format!("X-Vivi-Depends-On: {dep}\r\n"));
    }
    let eml = format!(
        "From: {}\r\nTo: {}\r\nSubject: {subject}\r\nX-Vivi-Kind: {kind}\r\n{headers}\r\n\
         done_when: {subject}\r\n",
        address(project, "mind"),
        address(project, "hand"),
    );
    let path = project.join(format!("{subject}.eml"));
    std::fs::write(&path, eml).unwrap();
    ok(
        project,
        &[
            "mail",
            "deliver",
            path.to_str().unwrap(),
            "--folder",
            folder,
        ],
    );
}

fn handle_of(project: &Path, kind: &str, subject: &str) -> String {
    let listing = ok(project, &[kind, "list", "--for", "hand"]);
    listing
        .lines()
        .find(|line| line.contains(subject))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("no {subject} in:\n{listing}"))
        .to_string()
}

#[test]
fn delivered_letters_with_dependency_headers_agree_across_graph_step_and_list() {
    let tmp = project();
    let p = tmp.path();
    // A first tracked item so the backlog graph exists.
    ok(
        p,
        &[
            "task",
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--subject",
            "seed",
            "--body",
            "done_when: seed",
        ],
    );
    deliver(p, "needs", "first", "need", &[]);
    let first = handle_of(p, "need", "first");
    deliver(p, "tasks", "second", "task", &[&first]);
    deliver(p, "tasks", "orphan", "task", &["deadbeef"]);

    // The nodes were minted at ingest, so the audit is clean with no repair run.
    let audit = ok(p, &["graph", "audit"]);
    assert!(audit.contains("missing=0"), "{audit}");
    assert!(audit.contains("edge=0"), "{audit}");
    assert!(!audit.contains("missing_node"), "{audit}");
    assert!(!audit.contains("[repaired]"), "{audit}");

    // A reference that never resolves blocks its dependent: the gap stays
    // visible in every projection instead of silently dispatching.
    let blocked = ok(p, &["task", "list", "--for", "hand", "--blocked"]);
    assert!(blocked.contains("second"), "{blocked}");
    assert!(blocked.contains("orphan"), "{blocked}");
    let ready = ok(p, &["graph", "ready"]);
    assert!(ready.contains("blocked=2"), "{ready}");
    let second = handle_of(p, "task", "second");
    let orphan = handle_of(p, "task", "orphan");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&ok(
        p,
        &["graph", "blocked", "backlog", "--kind", "task", "--json"],
    ))
    .unwrap();
    let row = rows
        .iter()
        .find(|row| row["source_id"] == second.as_str())
        .expect("the dependent task is listed as blocked");
    assert_eq!(row["blocked_by"], serde_json::json!([first]));
    let row = rows
        .iter()
        .find(|row| row["source_id"] == orphan.as_str())
        .expect("the orphan task is listed as blocked");
    assert_eq!(row["blocked_by"], serde_json::json!(["deadbeef"]));
    let step = ok(p, &["step"]);
    let (dispatches, exceptions) = step
        .split_once("  node  item  kind  reason  detail")
        .expect("the manifest prints an exception table");
    assert!(dispatches.contains("first"), "{step}");
    assert!(
        !dispatches.contains(&orphan),
        "the orphan must not dispatch:\n{step}"
    );
    assert!(!step.contains("second"), "{step}");
    let line = exceptions
        .lines()
        .find(|line| line.contains("awaiting_dependency"))
        .expect("the unheld dependency is reported as an exception");
    assert!(line.contains(&orphan), "{step}");
    assert!(line.contains("deadbeef"), "{step}");
}

#[test]
fn imported_letters_keep_their_backlog_nodes_and_edges() {
    let src = project();
    deliver(src.path(), "needs", "first", "need", &[]);
    let first = handle_of(src.path(), "need", "first");
    deliver(src.path(), "tasks", "second", "task", &[&first]);

    // Same roles in the target: the imported account rows are locally held.
    let target = project();
    ok(
        target.path(),
        &[
            "mailspace",
            "import",
            "--from",
            src.path().to_str().unwrap(),
        ],
    );

    // Import carries the minted nodes and edges, so no repair is ever needed.
    let audit = ok(target.path(), &["graph", "audit"]);
    assert!(audit.contains("missing=0"), "{audit}");
    assert!(!audit.contains("missing_node"), "{audit}");
    assert!(!audit.contains("[repaired]"), "{audit}");
    // The letters really landed in the target, so the block below is theirs.
    assert!(
        ok(target.path(), &["need", "list", "--for", "hand"]).contains("first"),
        "the imported need is held in the target"
    );
    assert!(
        ok(target.path(), &["task", "list", "--for", "hand"]).contains("second"),
        "the imported task is held in the target"
    );
    let ready = ok(target.path(), &["graph", "ready"]);
    assert!(ready.contains("blocked=1"), "{ready}");
}

fn ingest_held(mailspace: &Mailspace, account: &str, role: &str, subject: &str, kind: &str) {
    ingest(mailspace, account, role, subject, kind, &[]);
}

/// File a letter straight into the store: no node, as a route that attaches
/// nothing leaves it. Returns its display handle.
fn ingest(
    mailspace: &Mailspace,
    account: &str,
    role: &str,
    subject: &str,
    kind: &str,
    deps: &[&str],
) -> String {
    let headers: String = deps
        .iter()
        .map(|dep| format!("X-Vivi-Depends-On: {dep}\r\n"))
        .collect();
    let letter = format!(
        "From: mind@{domain}\r\nTo: {account}@{domain}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nX-Vivi-Kind: {kind}\r\n{headers}\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\ndone_when: {subject}\r\n",
        domain = mailspace.address_domain()
    );
    let mut storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("{account}\0{subject}"),
                remote: None,
            },
            letter.as_bytes(),
        )
        .unwrap();
    storage.display_handle(&stored.message_id).unwrap()
}

#[test]
fn drifted_edges_are_reported_and_repaired_without_a_server() {
    let tmp = project();
    let p = tmp.path();
    let mailspace = Mailspace::discover(Some(p)).unwrap();
    let need = ingest(&mailspace, "hand", "needs", "base", "need", &[]);
    let task = ingest(&mailspace, "hand", "tasks", "later", "task", &[&need]);
    // Nodes minted the way a pull did before edges were rebuilt.
    mailspace
        .backlog_attach("need", &need, "base", &[])
        .unwrap();
    mailspace
        .backlog_attach("task", &task, "later", &[])
        .unwrap();

    let list = ok(p, &["task", "list", "--for", "hand", "--blocked"]);
    assert!(
        list.contains("later"),
        "the header says blocked:
{list}"
    );
    assert!(ok(p, &["graph", "ready"]).contains("blocked=0"));
    let audit = ok(p, &["graph", "audit"]);
    assert!(audit.contains("edge=1"), "{audit}");
    assert!(audit.contains("missing_edge"), "{audit}");

    let repair = ok(p, &["graph", "audit", "--repair"]);
    assert!(repair.contains("[repaired]"), "{repair}");
    assert!(ok(p, &["graph", "ready"]).contains("blocked=1"));
    assert!(ok(p, &["graph", "audit"]).contains("edge=0"));
    assert!(!ok(p, &["step"]).contains("later"));
}

#[test]
fn letters_for_an_unknown_role_are_listed_not_dispatched_and_adopted_by_role_add() {
    let tmp = project();
    let p = tmp.path();
    let mailspace = Mailspace::discover(Some(p)).unwrap();
    ingest_held(&mailspace, "ghost", "tasks", "haunt", "task");
    ingest_held(&mailspace, "ghost", "inbox", "note", "mail");

    let status = ok(p, &["sync", "status"]);
    assert!(
        status.contains("held for unknown roles: ghost (2)"),
        "{status}"
    );
    let boot = ok(p, &["boot"]);
    assert!(boot.contains("held for unknown roles"), "{boot}");
    assert!(boot.contains("ghost"), "{boot}");
    assert!(
        !boot.contains("haunt"),
        "boot lists no dispatchable work: {boot}"
    );
    let step = ok(p, &["step"]);
    assert!(!step.contains("haunt"), "{step}");
    let totals = boot.lines().find(|l| l.starts_with("totals")).unwrap();
    assert!(totals.contains("open 0 task"), "{totals}");

    let added = ok(p, &["role", "add", "ghost", "--kind", "hand"]);
    assert!(added.contains("adopted 2 held letters"), "{added}");
    let step = ok(p, &["step"]);
    assert!(step.contains("haunt"), "{step}");
    let status = ok(p, &["sync", "status"]);
    assert!(!status.contains("unknown roles"), "{status}");
    let boot = ok(p, &["boot"]);
    assert!(!boot.contains("held for unknown roles"), "{boot}");
}

#[test]
fn renaming_a_role_to_a_held_name_adopts_loudly_too() {
    let tmp = project();
    let p = tmp.path();
    let mailspace = Mailspace::discover(Some(p)).unwrap();
    ingest_held(&mailspace, "scribe", "needs", "typo", "need");

    let renamed = ok(p, &["role", "rename", "hand", "scribe"]);
    assert!(renamed.contains("adopted 1 held letter"), "{renamed}");
    assert!(ok(p, &["step"]).contains("typo"));
}
