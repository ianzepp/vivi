use std::collections::BTreeSet;

use super::super::model::{RowKind, RowState};
use super::{merge_roots, snapshot};

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
    let slot = alpha.join("pool/alpha/lanes/slot-1");
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
    assert_eq!(got, vec![b, a]);
}
