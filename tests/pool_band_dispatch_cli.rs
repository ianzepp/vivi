//! The band path through the real CLI, as a baseline: a one-slot local
//! worktree pool whose band table holds a single entry and no caps still
//! dispatches exactly as the pool machinery always has. The band's only
//! choice is the pair the job runs at, the durable record on the task is the
//! trusted receipt of it, a role of kind `auditor` runs read-only under its
//! charter, and nothing is published.

#[path = "support/pool_band.rs"]
mod pool_band;

use vivi::pool::collect::READ_ONLY_UNCHANGED;
use vivi::pool::record::DispatchRecord;

use pool_band::{
    Rig, bands_table, dispatch, git_text, harness_table, pool_events, receipt_of, send_task,
    task_body, wait_settled,
};

/// The charter the test sets on the auditor role: two plain lines and the
/// v20 reply envelope a charter must ask for.
const CHARTER: &str =
    "Audit faithfully.\nReport every finding.\nReply inside a v20 envelope:\n+++\n";

#[test]
fn single_entry_without_limits_preserves_existing_dispatch() {
    // A single-entry band, no caps table, and a harness that renders the
    // chosen pair and cats the standing rules it was handed.
    let rig = Rig::new(
        &bands_table(&[("P1", &[("standin", "mid")])]),
        "",
        &harness_table(
            "echo model={model} reasoning={reasoning}; cat {rules}",
            &["standin"],
        ),
    );
    rig.add_role("auditor", "auditor", Some("P1"));
    rig.set_charter("auditor", CHARTER);
    rig.create_pool();

    let task = send_task(
        &rig,
        "auditor",
        "Audit the tree",
        &task_body("Report what you see.", Some("main"), None, None, &[]),
    );
    let (sent, said) = dispatch(&rig, &task, &["--from", "main"]);
    assert!(sent, "{said}");
    assert!(said.contains("model standin"), "{said}");
    assert!(said.contains("read-only"), "{said}");
    assert!(
        said.contains("role auditor band P1 -> standin mid read-only (role kind auditor)"),
        "{said}"
    );

    let settled = wait_settled(&rig, &task);
    let line = settled
        .lines()
        .find(|line| line.starts_with(&task))
        .expect("a settled line");
    assert!(line.contains("finished"), "{settled}");
    assert!(line.contains(READ_ONLY_UNCHANGED), "{settled}");

    // The chosen pair is the band's single entry, rendered by the harness.
    let receipt = receipt_of(&rig, &task);
    assert!(receipt.contains("model=standin reasoning=mid"), "{receipt}");

    // The rules the job was handed: the charter above the pool rules.
    let charter = receipt
        .find("# Standing charter (role auditor)")
        .expect("a charter heading");
    let rules = receipt.find("# Pool rules").expect("a pool rules heading");
    assert!(charter < rules, "{receipt}");
    assert!(receipt.contains("Audit faithfully."), "{receipt}");

    // The trusted dispatch record on the task: what ran, read-only, no
    // branch, from the commit `main` named.
    let events = pool_events(&rig);
    let dispatched = events
        .iter()
        .find(|event| event.handle == task && event.event_type == "pool_dispatched")
        .expect("a pool_dispatched record");
    let record: DispatchRecord =
        serde_json::from_str(&dispatched.note).expect("a dispatch record note");
    assert_eq!(record.model, "standin");
    assert_eq!(record.pool, "wt");
    assert_eq!(record.from, "main");
    assert!(record.read_only, "{record:?}");
    assert!(record.branch.is_empty(), "{record:?}");

    // The collected record closes the dispatch: finished, read-only, and no
    // publication anywhere on it.
    let collected = events
        .iter()
        .find(|event| event.handle == task && event.event_type == "pool_collected")
        .expect("a pool_collected record");
    let note: serde_json::Value = serde_json::from_str(&collected.note).expect("a note");
    assert_eq!(note["outcome"], "finished");
    assert_eq!(note["model"], "standin");
    assert_eq!(note["read_only"], true);
    assert_eq!(note["push"], READ_ONLY_UNCHANGED);
    assert_eq!(note["agent_rc"], "0");
    assert_eq!(note["branch"], "");
    assert_eq!(note["sha"], "");
    assert_eq!(note["publish_ref"], serde_json::Value::Null);

    // Nothing moved in the repository either: only the scaffold's two heads.
    let heads = git_text(
        &rig.repo,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    );
    assert_eq!(heads.lines().count(), 2, "{heads}");
}
