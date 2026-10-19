//! Tests for the disk section of `vivi doctor`.

use super::*;
use crate::doctor::Status;
use crate::mailspace::Mailspace;

/// A git repository with `main`, `unit/x` and `review/aaa`; returns the
/// directory and the repository path. Mirrors the helper in
/// `local_pool_work_command_test.rs`.
fn repo_with_branches() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@localhost")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@localhost")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    git(&["branch", "unit/x"]);
    git(&["branch", "review/aaa"]);
    (dir, repo)
}

#[test]
fn dir_size_sums_files_and_ignores_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.bin"), [0u8; 100]).unwrap();
    std::fs::write(dir.path().join("b.bin"), [0u8; 250]).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path().join("a.bin"), dir.path().join("a-link")).unwrap();
    assert_eq!(dir_size_bytes(dir.path()), 350);
}

#[test]
fn size_finding_warns_above_threshold_and_ok_below() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.bin");
    std::fs::write(&file, [0u8; 100]).unwrap();
    let warn = size_finding(SECTION, "disk use", &file, 50);
    assert_eq!(warn.status, Status::Warn, "{warn:#?}");
    assert_eq!(
        warn.detail,
        format!("{} is 0.0 GiB (warns above 0 GiB)", file.display())
    );
    assert_eq!(warn.fix.as_deref(), Some(RECLAIM_FIX));
    let ok = size_finding(SECTION, "disk use", &file, 100);
    assert_eq!(ok.status, Status::Ok, "{ok:#?}");
    assert_eq!(ok.detail, format!("{} is 0.0 GiB", file.display()));
    assert!(ok.fix.is_none(), "{ok:#?}");
}

#[test]
fn missing_path_skips() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nowhere");
    let finding = size_finding(
        SECTION,
        "pump home",
        &missing,
        PUMP_HOME_WARN_GIB * BYTES_PER_GIB,
    );
    assert_eq!(finding.status, Status::Skipped, "{finding:#?}");
    assert_eq!(finding.detail, "pump home absent");
    assert!(finding.fix.is_none(), "{finding:#?}");
}

#[test]
fn branch_count_counts_unit_refs() {
    let (_dir, repo) = repo_with_branches();
    assert_eq!(branch_count(&repo), Some(1));
}

#[test]
fn no_pools_skips_pool_roots() {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let out = findings(tmp.path());
    let pools = out
        .iter()
        .find(|finding| finding.name == "pools")
        .unwrap_or_else(|| panic!("no pools finding in {out:#?}"));
    assert_eq!(pools.section, SECTION);
    assert_eq!(pools.status, Status::Skipped, "{pools:#?}");
    assert!(
        out.iter().all(|finding| !finding.name.starts_with("pool root")),
        "{out:#?}"
    );
}
