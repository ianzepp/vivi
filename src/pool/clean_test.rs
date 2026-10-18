use super::*;
use crate::pool::clean_rule::{write_baseline, write_mark};
use crate::pump::state::{Status, Unit};
use std::fs;

#[test]
fn after_merged_is_a_noop_without_a_mailspace() {
    let tmp = tempfile::tempdir().unwrap();
    let unit = Unit {
        status: Status::Merged,
        task: Some("abcd1234".into()),
        pool: Some("lanes".into()),
        ..Unit::default()
    };
    let report = after_merged(tmp.path(), &unit).unwrap();
    assert!(report.actions.is_empty());
    assert!(report.skips.is_empty());
}

#[test]
fn after_slot_freed_reclaims_an_over_cap_target() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("slot");
    fs::create_dir_all(dir.join("marks")).unwrap();
    fs::write(dir.join("marks/vivi_pool"), "lanes\n").unwrap();
    write_mark(&dir, "vivi_pool_state", "ready").unwrap();
    write_baseline(&dir, 10).unwrap();
    fs::create_dir_all(dir.join("repo/src")).unwrap();
    fs::write(dir.join("repo/src/lib.rs"), "ok\n").unwrap();
    fs::create_dir_all(dir.join("repo/target")).unwrap();
    fs::write(dir.join("repo/target/blob"), vec![0_u8; 50]).unwrap();
    let report = after_slot_freed(&dir, Some("target")).unwrap();
    assert_eq!(report.actions.len(), 1);
    assert_eq!(report.actions[0].kind, ActionKind::Build);
    assert!(!dir.join("repo/target/blob").exists());
    assert_eq!(
        fs::read_to_string(dir.join("repo/src/lib.rs")).unwrap(),
        "ok\n"
    );
}

#[test]
fn a_claimed_slot_is_skipped_after_free_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("slot");
    fs::create_dir_all(dir.join("marks")).unwrap();
    fs::write(dir.join("marks/vivi_pool"), "lanes\n").unwrap();
    write_mark(&dir, "vivi_pool_state", "ready").unwrap();
    fs::write(dir.join("claim"), "task=abcd\n").unwrap();
    fs::create_dir_all(dir.join("repo/target")).unwrap();
    fs::write(dir.join("repo/target/blob"), vec![0_u8; 50]).unwrap();
    write_baseline(&dir, 10).unwrap();
    let report = after_slot_freed(&dir, Some("target")).unwrap();
    assert!(report.actions.is_empty());
    assert!(dir.join("repo/target/blob").is_file());
}
