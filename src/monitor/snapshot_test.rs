use std::collections::BTreeSet;
use std::path::Path;

use super::super::model::{RowKind, RowState};
use super::{merge_roots, snapshot};

const LANES_POOL: &str =
    "[pools.lanes]\nprovider='local'\nslots=2\n[pools.lanes.local]\nroot='pool'\n";
const BAD_POOL: &str =
    "[pools.bad]\nprovider='local'\nslots=1\n[pools.bad.local]\nroot='../outside'\n";

fn write_project(root: &Path, config: &str) {
    std::fs::create_dir_all(root.join(".vivi")).unwrap();
    std::fs::write(root.join(".vivi/mailspace.toml"), config).unwrap();
}

/// A claimed local slot under `<root>/pool/lanes/slot-1` that yields a
/// monitor row when the project's sources read cleanly. The mailspace mark
/// must match the mailspace name for the slot to be owned.
fn write_claimed_slot(root: &Path, mailspace: &str) {
    let slot = root.join("pool/lanes/slot-1");
    std::fs::create_dir_all(slot.join("marks")).unwrap();
    for (name, value) in [
        ("vivi_pool", "lanes"),
        ("vivi_pool_mailspace", mailspace),
        ("vivi_pool_slot", "1"),
        ("vivi_pool_state", "ready"),
    ] {
        std::fs::write(slot.join("marks").join(name), format!("{value}\n")).unwrap();
    }
    std::fs::write(slot.join("claim"), "task=hand-12345678\nclaimed=1000\n").unwrap();
}

#[test]
fn snapshot_reads_projects_without_opening_write_capable_mailspace() {
    let temp = tempfile::tempdir().unwrap();
    let alpha = temp.path().join("alpha");
    let beta = temp.path().join("beta");
    let projects = [
        (
            &alpha,
            "name='alpha'\n[pools.lanes]\nprovider='local'\nslots=2\n[pools.lanes.local]\nroot='pool'\n[pump]\n",
        ),
        (&beta, "name='beta'\n"),
    ];
    for (root, config) in projects {
        std::fs::create_dir_all(root.join(".vivi")).unwrap();
        std::fs::write(root.join(".vivi/mailspace.toml"), config).unwrap();
    }
    let slot = alpha.join("pool/lanes/slot-1");
    std::fs::create_dir_all(slot.join("marks")).unwrap();
    for (name, value) in [
        ("vivi_pool", "lanes"),
        ("vivi_pool_mailspace", "alpha"),
        ("vivi_pool_slot", "1"),
        ("vivi_pool_state", "ready"),
    ] {
        std::fs::write(slot.join("marks").join(name), format!("{value}\n")).unwrap();
    }
    std::fs::write(slot.join("claim"), "task=hand-12345678\nclaimed=1000\n").unwrap();
    let result = snapshot(&[alpha.clone(), beta], 1060);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert_eq!(result.active_failures, 0);
    assert!(
        result
            .rows
            .iter()
            .any(|row| row.project == "alpha" && row.kind == RowKind::Slot)
    );
    assert_eq!(
        result
            .rows
            .iter()
            .filter(|row| row.kind == RowKind::Pump)
            .count(),
        1
    );
    assert!(
        result
            .rows
            .iter()
            .any(|row| row.state == RowState::Finished("uncollected".into()))
    );
    assert!(!alpha.join(".vivi/mailspace.id").exists());
}

#[test]
fn registered_roots_precede_deduplicated_extra_roots() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let got = merge_roots(BTreeSet::from([b.clone()]), &[b.join("."), a.clone()]);
    assert_eq!(
        got,
        vec![b.canonicalize().unwrap(), a.canonicalize().unwrap()]
    );
}

#[test]
fn a_corrupt_event_store_fails_the_project_without_partial_rows() {
    let temp = tempfile::tempdir().unwrap();
    let alpha = temp.path().join("alpha");
    write_project(&alpha, &format!("name='alpha'\n{LANES_POOL}"));
    // The same claimed-slot fixture yields a row in the healthy test above,
    // so any surviving row here would be a partial read leaking through.
    write_claimed_slot(&alpha, "alpha");
    std::fs::write(alpha.join(".vivi/mail.sqlite"), b"corrupt, not a database").unwrap();
    let result = snapshot(&[alpha], 1060);
    assert_eq!(result.active_failures, 1);
    assert!(result.rows.is_empty(), "{:?}", result.rows);
    assert_eq!(result.warnings.len(), 1);
    assert!(
        result.warnings[0].starts_with("alpha: "),
        "{}",
        result.warnings[0]
    );
    assert!(
        result.warnings[0].contains("events"),
        "{}",
        result.warnings[0]
    );
}

#[test]
fn multiple_failing_sources_become_one_project_warning() {
    let temp = tempfile::tempdir().unwrap();
    let alpha = temp.path().join("alpha");
    write_project(
        &alpha,
        &format!("name='alpha'\n{LANES_POOL}{BAD_POOL}[pump]\n"),
    );
    std::fs::write(alpha.join(".vivi/mail.sqlite"), b"corrupt, not a database").unwrap();
    std::fs::create_dir_all(alpha.join(".vivi/pump")).unwrap();
    std::fs::write(
        alpha.join(".vivi/pump/monitor-cycles.jsonl"),
        b"{not json}\n",
    )
    .unwrap();
    let result = snapshot(&[alpha], 1060);
    assert_eq!(result.active_failures, 1);
    assert!(result.rows.is_empty(), "{:?}", result.rows);
    assert_eq!(result.warnings.len(), 1);
    let warning = &result.warnings[0];
    assert!(warning.starts_with("alpha: "), "{warning}");
    assert!(warning.contains("events"), "{warning}");
    assert!(warning.contains("bad"), "{warning}");
    assert!(warning.contains("pump cycles"), "{warning}");
}

#[test]
fn a_failing_slot_source_discards_the_projects_rows() {
    let temp = tempfile::tempdir().unwrap();
    let alpha = temp.path().join("alpha");
    write_project(&alpha, &format!("name='alpha'\n{LANES_POOL}{BAD_POOL}"));
    write_claimed_slot(&alpha, "alpha");
    let result = snapshot(&[alpha], 1060);
    assert_eq!(result.active_failures, 1);
    assert!(result.rows.is_empty(), "{:?}", result.rows);
    assert_eq!(result.warnings.len(), 1);
    let warning = &result.warnings[0];
    assert!(warning.starts_with("alpha: "), "{warning}");
    assert!(warning.contains("bad"), "{warning}");
    assert!(!warning.contains("alpha (bad)"), "{warning}");
}

#[test]
fn a_failed_project_does_not_stop_a_healthy_neighbor() {
    let temp = tempfile::tempdir().unwrap();
    let alpha = temp.path().join("alpha");
    let beta = temp.path().join("beta");
    write_project(&alpha, "name='alpha'\n");
    write_project(&beta, &format!("name='beta'\n{LANES_POOL}"));
    write_claimed_slot(&beta, "beta");
    std::fs::write(alpha.join(".vivi/mail.sqlite"), b"corrupt, not a database").unwrap();
    let result = snapshot(&[alpha, beta], 1060);
    assert_eq!(result.active_failures, 1);
    assert_eq!(result.warnings.len(), 1);
    assert!(
        result.warnings[0].starts_with("alpha: "),
        "{}",
        result.warnings[0]
    );
    assert!(!result.rows.is_empty(), "{:?}", result.rows);
    assert!(result.rows.iter().all(|row| row.project == "beta"));
}
