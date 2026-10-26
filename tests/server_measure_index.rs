//! Measurement, not assertion: hosted-index passes over isolated host
//! mailspaces of several thousand letters, and the child writers that can
//! run beside them.
//!
//! Each mailspace is built with the vivi library's own `send`: a quarter of
//! the letters addressed off the host as needs, a quarter off the host as
//! mail, and half to the host's own `hand` (local only under
//! `send = "addressed"`). Two isolated roots are served by one
//! `ProjectStore`; the measurement tests are ignored by default; run with:
//!
//! ```text
//! cargo test --release --test server_measure_index -- --ignored --nocapture
//! ```
//!
//! `VIVI_MEASURE_LETTERS` overrides the per-root letter count (default
//! 5000).
//!
//! Child writers are separate processes the parent launches through the
//! `writer_child_run` entry point in `tests/server_measure_index/fixture.rs`.
//! A writer adds letters and makes lifecycle changes on a fixed, recorded
//! schedule with a bounded duration, in a letters mode (sends plus
//! close/reopen) and a writer-overhead mode (verification-only binding
//! updates: a unit lowered onto a need, closed, joined). Writers see only
//! their mailspace root and their own `VIVI_CONFIG_DIR`; they never share
//! the measurement's server process or its `ProjectStore`.

#[path = "server_measure_index/fixture.rs"]
mod fixture;

use std::collections::BTreeSet;
use std::path::Path;

use vivi::server::hosted;
use vivi::server::identities::IdentityRegistry;
use vivi::server::projects::{IndexOutcome, ProjectStore};

use fixture::{
    OFF_HOST, WriterConfig, WriterMode, scratch_base, seeded_root, send_letter, snapshot, timed,
};

fn measure_letters() -> usize {
    std::env::var("VIVI_MEASURE_LETTERS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(5000)
}

#[test]
#[ignore = "measurement; builds two hosted roots of thousands of letters"]
fn index_passes_over_two_isolated_roots() {
    let letters = measure_letters();
    let base = scratch_base("server-measure-index");
    let root_a = base.join("host-a");
    let root_b = base.join("host-b");

    let (ms_a, build_a) = timed(&format!("build root a: {letters} letters"), || {
        seeded_root(&root_a, letters, "hub-a")
    });
    let (ms_b, build_b) = timed(&format!("build root b: {letters} letters"), || {
        seeded_root(&root_b, letters, "hub-b")
    });
    for took in [build_a, build_b] {
        println!(
            "  ({:.2} ms per letter)",
            took.as_secs_f64() * 1000.0 / letters as f64
        );
    }

    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (projects, identities) = (ProjectStore::new(&data), IdentityRegistry::new(&data));
    let report = hosted::reconcile(
        &projects,
        &identities,
        &BTreeSet::from([root_a.clone(), root_b.clone()]),
    );
    assert!(report.refused.is_empty(), "{report:?}");
    assert_eq!(report.served.len(), 2, "{report:?}");
    let id_of = |root: &Path| {
        report
            .served
            .iter()
            .find(|served| served.root == root)
            .unwrap()
            .project_id
            .clone()
    };
    let (id_a, id_b) = (id_of(&root_a), id_of(&root_b));

    for (name, id) in [("root a", &id_a), ("root b", &id_b)] {
        let (outcome, _) = timed(
            &format!("first pass, {name}: index the off-host half"),
            || projects.index_hosted(id).unwrap(),
        );
        println!("  {outcome:?}");
    }

    // Unchanged mailspaces: watermark checks, no scan.
    for (name, id) in [("root a", &id_a), ("root b", &id_b)] {
        let (outcome, _) = timed(&format!("pass with nothing changed, {name}"), || {
            projects.index_hosted(id).unwrap()
        });
        println!("  {outcome:?}");
    }

    // One new off-host letter on root a moves its watermark: the pass scans
    // every message row again to find it. On root b, one writer-style churn
    // group moves its own watermark (two of its four letters are off-host).
    send_letter(&ms_a, OFF_HOST, Some("need"), &format!("letter {letters}"));
    fixture::churn_group(&ms_b, "measure");

    let (outcome, _) = timed("steady pass, root a, one new letter", || {
        projects.index_hosted(&id_a).unwrap()
    });
    println!("  {outcome:?}");
    assert!(
        matches!(outcome, IndexOutcome::Ran { indexed: 1, .. }),
        "{outcome:?}"
    );
    let (outcome, _) = timed("steady pass, root b, one writer group", || {
        projects.index_hosted(&id_b).unwrap()
    });
    println!("  {outcome:?}");
    assert!(
        matches!(outcome, IndexOutcome::Ran { indexed: 2, .. }),
        "{outcome:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// The entry point another test process launches as a child writer:
///
/// ```text
/// <test-binary> --exact writer_child_run --ignored --nocapture
/// ```
///
/// with the schedule in `VIVI_WRITER_*` variables (see the fixture).
#[test]
#[ignore = "child-writer entry point; launched by another test process"]
fn writer_child_run() {
    fixture::child_writer_main();
}

/// A short child-writer run, asserted against the storage it leaves behind.
/// No CPU assertions: only the recorded schedule and the storage deltas it
/// must produce, first in letters mode, then in writer-overhead mode.
#[test]
fn child_writer_short_run_moves_storage() {
    let base = scratch_base("server-measure-writer");
    let root = base.join("host");
    let seeded = seeded_root(&root, 8, "hub-writer");
    drop(seeded);
    let config_dir = base.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let before = snapshot(&root);
    let receipt_path = base.join("letters-receipt.json");
    let config =
        WriterConfig::short_run(&root, &receipt_path, WriterMode::Letters, "short-letters");
    let output = run_child(&config, &config_dir);
    println!("child writer output:\n{output}");
    let receipt = fixture::read_receipt(&receipt_path);
    assert_eq!(receipt.mode, "letters");
    assert_eq!(receipt.ticks_ran, 2);
    assert_eq!(receipt.letters_sent, 8);
    assert_eq!(receipt.offhost_letters, 4);
    assert_eq!(receipt.local_letters, 4);
    assert_eq!(receipt.closes, 1);
    assert_eq!(receipt.reopens, 0);
    assert_eq!(receipt.binds, 0);

    let after = snapshot(&root);
    // Two local needs sent, one closed: one stays open.
    assert_eq!(after.needs_open - before.needs_open, 1, "needs delta");
    // Two local letters, unread.
    assert_eq!(after.inbox_unread - before.inbox_unread, 2, "inbox delta");
    // The closed need moved to done.
    assert_eq!(after.done - before.done, 1, "done delta");
    assert_eq!(after.tasks_open - before.tasks_open, 0, "tasks delta");
    // Four delivered letters plus the close event letter.
    assert_eq!(after.hand_rows - before.hand_rows, 5, "hand rows delta");
    // Eight sent copies plus the close event letter's sent copy.
    assert_eq!(after.mind_rows - before.mind_rows, 9, "mind rows delta");
    assert_receipt_nodes(&receipt, &after);

    // The same mailspace under a writer-overhead run: verification-only
    // binding updates. Each tick lowers one unit onto one need, closes the
    // unit, and the join completes the need.
    let before = after;
    let receipt_path = base.join("overhead-receipt.json");
    let config =
        WriterConfig::short_run(&root, &receipt_path, WriterMode::Overhead, "short-overhead");
    let output = run_child(&config, &config_dir);
    println!("child writer output:\n{output}");
    let receipt = fixture::read_receipt(&receipt_path);
    assert_eq!(receipt.mode, "overhead");
    assert_eq!(receipt.ticks_ran, 2);
    assert_eq!(receipt.binds, 2);
    assert_eq!(receipt.joined_needs, 2);

    let after = snapshot(&root);
    // Both the need and the unit end done, so no open folder grows.
    assert_eq!(after.needs_open - before.needs_open, 0, "needs delta");
    assert_eq!(after.tasks_open - before.tasks_open, 0, "tasks delta");
    assert_eq!(after.inbox_unread - before.inbox_unread, 0, "inbox delta");
    assert_eq!(after.done - before.done, 4, "done delta");
    // Per tick: need, unit, bound event letter, done event letter.
    assert_eq!(after.hand_rows - before.hand_rows, 8, "hand rows delta");
    assert_eq!(after.mind_rows - before.mind_rows, 8, "mind rows delta");
    for item in &receipt.items {
        let state = after
            .nodes
            .get(&item.item)
            .unwrap_or_else(|| panic!("no backlog node for {item:?}"));
        assert_eq!(*state, "done", "{item:?}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

fn run_child(config: &WriterConfig, config_dir: &Path) -> String {
    let mut child = fixture::spawn_child_writer(config, config_dir);
    fixture::wait_for(&mut child, config.budget()).unwrap_or_else(|error| panic!("{error}"))
}

/// The backlog graph matches the receipt: work items minted nodes in the
/// state the schedule left them in, mail minted none.
fn assert_receipt_nodes(receipt: &fixture::WriterReceipt, after: &fixture::StorageSnapshot) {
    for item in &receipt.items {
        let state = after.nodes.get(&item.item);
        if item.kind == "mail" {
            assert!(state.is_none(), "mail minted a backlog node: {item:?}");
            continue;
        }
        let state = state.unwrap_or_else(|| panic!("no backlog node for {item:?}"));
        let expected = if item.closed && !item.reopened {
            "done"
        } else {
            "open"
        };
        assert_eq!(*state, expected, "{item:?}");
    }
}
