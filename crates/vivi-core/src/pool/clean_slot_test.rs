use super::*;
use crate::pool::clean_rule::{
    DEFAULT_BASELINE_BYTES, GIB, read_baseline, write_baseline, write_mark,
};
use std::fs;

fn slot(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("marks")).unwrap();
    fs::write(dir.join("marks/vivi_pool"), "lanes\n").unwrap();
    write_mark(&dir, "vivi_pool_state", "ready").unwrap();
    dir
}

fn fat_target(dir: &Path, bytes: usize) {
    let target = dir.join("repo").join("target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("blob"), vec![0_u8; bytes]).unwrap();
}

#[test]
fn a_busy_slot_is_not_touched() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "busy");
    fat_target(&dir, 100);
    fs::write(dir.join("claim"), "task=abcd\n").unwrap();
    write_baseline(&dir, 10).unwrap();
    let (acts, skips) = reclaim_builds("p", "s", &dir, Some("target"), None, false).unwrap();
    assert!(acts.is_empty());
    assert_eq!(skips[0].reason, "slot is mid-dispatch");
    assert!(dir.join("repo/target/blob").is_file());
}

#[test]
fn reclaim_builds_holds_the_slot_and_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "held");
    fat_target(&dir, 80);
    write_baseline(&dir, 10).unwrap();
    let (acts, skips) = reclaim_builds("p", "s", &dir, Some("target"), None, false).unwrap();
    assert!(skips.is_empty(), "{skips:?}");
    assert_eq!(acts.len(), 1);
    assert!(!dir.join("repo/target/blob").exists());
    assert!(!dir.join("reclaim").exists(), "the hold must be released");
}

#[test]
fn reclaim_builds_skips_while_another_reclaim_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "held-elsewhere");
    fat_target(&dir, 80);
    write_baseline(&dir, 10).unwrap();
    fs::write(dir.join("reclaim"), "nonce=other\n").unwrap();
    let (acts, skips) = reclaim_builds("p", "s", &dir, Some("target"), None, false).unwrap();
    assert!(acts.is_empty());
    assert_eq!(skips.len(), 1, "{skips:?}");
    assert!(dir.join("repo/target/blob").is_file());
    assert!(
        dir.join("reclaim").is_file(),
        "the planted hold must survive"
    );
}

#[test]
fn an_over_cap_build_dir_is_emptied_and_source_is_left() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "fat");
    fs::create_dir_all(dir.join("repo/src")).unwrap();
    fs::write(dir.join("repo/src/lib.rs"), "fn x() {}\n").unwrap();
    fat_target(&dir, 250);
    write_baseline(&dir, 100).unwrap();
    let (acts, skips) = reclaim_builds("p", "s", &dir, Some("target"), None, false).unwrap();
    assert!(skips.is_empty(), "{skips:?}");
    assert_eq!(acts.len(), 1);
    assert_eq!(acts[0].bytes, 250);
    assert!(!dir.join("repo/target/blob").exists());
    assert_eq!(
        fs::read_to_string(dir.join("repo/src/lib.rs")).unwrap(),
        "fn x() {}\n"
    );
}

#[test]
fn dry_run_reports_bytes_and_deletes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "dry");
    fat_target(&dir, 250);
    write_baseline(&dir, 100).unwrap();
    let (acts, _) = reclaim_builds("p", "s", &dir, Some("target"), None, true).unwrap();
    assert_eq!(acts[0].bytes, 250);
    assert!(dir.join("repo/target/blob").is_file());
}

#[test]
fn a_missing_baseline_uses_the_default_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "old");
    // Just under 2x the 4 GiB hint: keep. The default exists so a first job
    // of a few hundred megabytes is not wiped.
    fat_target(&dir, 100);
    let (acts, _) = reclaim_builds("p", "s", &dir, Some("target"), None, false).unwrap();
    assert!(acts.is_empty());
    assert!(dir.join("repo/target/blob").is_file());
    assert_eq!(read_baseline(&dir), Some(100));
    assert!(DEFAULT_BASELINE_BYTES > 100);
    let _ = GIB;
}

#[test]
fn reclaim_builds_uses_the_pool_default_for_unmarked_slots() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "pool-default");
    fat_target(&dir, 80);
    let (acts, skips) = reclaim_builds("p", "s", &dir, Some("target"), Some(10), false).unwrap();
    assert!(skips.is_empty(), "{skips:?}");
    assert_eq!(acts.len(), 1);
    assert!(!dir.join("repo/target/blob").exists());
    assert_eq!(read_baseline(&dir), Some(10));

    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "pool-default-off");
    fat_target(&dir, 80);
    let (acts, _) = reclaim_builds("p", "s", &dir, Some("target"), None, false).unwrap();
    assert!(acts.is_empty(), "{acts:?}");
    assert!(dir.join("repo/target/blob").is_file());
    assert_eq!(read_baseline(&dir), Some(80));
}

#[test]
fn record_seed_baseline_stores_the_first_nonzero_size() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = slot(tmp.path(), "seed");
    fat_target(&dir, 80);
    record_seed_baseline(&dir, Some("target")).unwrap();
    assert_eq!(read_baseline(&dir), Some(80));
    fat_target(&dir, 200);
    record_seed_baseline(&dir, Some("target")).unwrap();
    assert_eq!(read_baseline(&dir), Some(80), "later growth is not a seed");
}
