//! A throwaway Git repository for the tests of the `local` provider. Never a
//! real repository: it lives in a temporary directory with two commits.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The temporary directory and the repository inside it.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    /// The primary checkout, on branch `main`, with a canonical path.
    pub repo: PathBuf,
}

/// Run `git` in `dir`; its trimmed output. Panics when it fails.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository on `main` with two commits, a `.gitignore` for `build/`, and
/// an identity for commits.
pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = root.join("project");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Throwaway"]);
    git(&repo, &["config", "user.email", "throwaway@localhost"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "build/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "second"]);
    Fixture { dir, repo }
}
