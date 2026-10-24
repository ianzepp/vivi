use super::*;
use crate::doctor::Status;

#[test]
fn directory_size_sums_files_and_ignores_symlinks() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a"), [0_u8; 3]).expect("file a");
    std::fs::write(temp.path().join("b"), [0_u8; 7]).expect("file b");
    #[cfg(unix)]
    std::os::unix::fs::symlink(temp.path().join("a"), temp.path().join("link")).expect("symlink");
    assert_eq!(dir_size_bytes(temp.path()), Some(10));
}

#[test]
fn size_findings_report_missing_small_and_large_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing = size_finding("disk", "test", &temp.path().join("missing"), 1);
    assert_eq!(missing.status, Status::Skipped);

    let small_path = temp.path().join("small");
    std::fs::create_dir(&small_path).expect("small dir");
    std::fs::write(small_path.join("data"), [0_u8; 1]).expect("small file");
    assert_eq!(
        size_finding("disk", "test", &small_path, 2).status,
        Status::Ok
    );

    let large_path = temp.path().join("large");
    std::fs::create_dir(&large_path).expect("large dir");
    std::fs::write(large_path.join("data"), [0_u8; 3]).expect("large file");
    assert_eq!(
        size_finding("disk", "test", &large_path, 2).status,
        Status::Warn
    );
}

#[test]
fn size_finding_stops_counting_once_over_the_threshold() {
    let temp = tempfile::tempdir().expect("tempdir");
    for index in 0..50 {
        std::fs::write(temp.path().join(format!("f{index}")), [0_u8; 10]).expect("file");
    }
    let finding = size_finding("disk", "test", temp.path(), 25);
    assert_eq!(finding.status, Status::Warn);
    assert!(finding.detail.contains("is over"), "{}", finding.detail);
    assert_eq!(dir_size_bytes(temp.path()), Some(500));
}
