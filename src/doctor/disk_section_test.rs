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
fn branch_count_counts_only_unit_refs() {
    let temp = tempfile::tempdir().expect("tempdir");
    git(temp.path(), &["init", "--quiet"]);
    git(
        temp.path(),
        &[
            "-c",
            "user.name=Doctor",
            "-c",
            "user.email=doctor@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "init",
            "--quiet",
        ],
    );
    git(temp.path(), &["branch", "vivi/unit/one"]);
    git(temp.path(), &["branch", "feature/other"]);
    assert_eq!(branch_count(temp.path()), Some(1));
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git available");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
