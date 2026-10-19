use super::*;
use std::os::unix::fs::symlink;

#[test]
fn twice_the_baseline_is_the_cap() {
    assert!(!over_cap(0, 100));
    assert!(!over_cap(200, 100));
    assert!(over_cap(201, 100));
    assert!(!over_cap(50, 0), "a zero baseline never trips");
}

#[test]
fn a_missing_mark_uses_the_default_fresh_build_hint() {
    assert_eq!(effective_baseline(None, None), DEFAULT_BASELINE_BYTES);
    assert_eq!(effective_baseline(Some(3 * GIB), None), 3 * GIB);
}

#[test]
fn the_pool_default_covers_only_unmarked_slots() {
    assert_eq!(effective_baseline(None, Some(10)), 10);
    assert_eq!(effective_baseline(Some(3 * GIB), Some(10)), 3 * GIB);
}

#[test]
fn dir_bytes_skips_symlinks_and_sums_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tree");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a"), vec![0_u8; 100]).unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/b"), vec![0_u8; 50]).unwrap();
    let outside = dir.path().join("outside");
    fs::write(&outside, vec![0_u8; 10_000]).unwrap();
    symlink(&outside, root.join("link")).unwrap();
    assert_eq!(dir_bytes(&root), 150);
}

#[test]
fn empty_dir_removes_children_and_keeps_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("target");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("obj"), vec![0_u8; 20]).unwrap();
    assert_eq!(empty_dir(&root).unwrap(), 20);
    assert!(root.is_dir());
    assert!(fs::read_dir(&root).unwrap().next().is_none());
    assert_eq!(empty_dir(&root.join("missing")).unwrap(), 0);
}

#[test]
fn baseline_marks_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let slot = dir.path();
    assert_eq!(read_baseline(slot), None);
    write_baseline(slot, 42).unwrap();
    assert_eq!(read_baseline(slot), Some(42));
}

#[test]
fn format_bytes_picks_a_readable_unit() {
    assert_eq!(format_bytes(3 * GIB + GIB / 2), "3.5 GiB");
    assert!(format_bytes(800 * 1024 * 1024).contains("MiB"));
    assert!(format_bytes(4096).contains("KiB"));
}

#[test]
fn build_paths_names_the_configured_dir_and_cargo_target() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    fs::create_dir(repo.join("build")).unwrap();
    fs::create_dir(repo.join("target")).unwrap();
    let paths = build_paths(repo, Some("build"));
    assert_eq!(paths.len(), 2);
    assert!(paths.iter().any(|p| p.ends_with("build")));
    assert!(paths.iter().any(|p| p.ends_with("target")));
}
