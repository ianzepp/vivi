use std::process::Command;

use super::*;
use crate::pool::local_fixture::{fixture, git};

#[test]
fn the_porcelain_listing_reads_branches_detached_and_bare_worktrees() {
    let text = "worktree /a/primary\nHEAD abc\nbranch refs/heads/main\n\nworktree /a/slot-1/repo\nHEAD def\ndetached\n\nworktree /a/bare\nbare\n";
    let list = parse_worktrees(text);
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].branch.as_deref(), Some("main"));
    assert_eq!(list[1].branch, None);
    assert!(!list[1].bare && list[2].bare);
    assert_eq!(list[1].path, PathBuf::from("/a/slot-1/repo"));
}

#[test]
fn a_branch_with_slashes_keeps_its_name() {
    let list = parse_worktrees("worktree /x\nHEAD a\nbranch refs/heads/factory/job-1\n");
    assert_eq!(list[0].branch.as_deref(), Some("factory/job-1"));
}

#[test]
fn the_repository_is_read_without_changing_it() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let trees = repo.worktrees().unwrap();
    assert_eq!(trees.len(), 1);
    assert_eq!(trees[0].path, f.repo);
    assert_eq!(trees[0].branch.as_deref(), Some("main"));
    let tip = git(&f.repo, &["rev-parse", "HEAD"]);
    assert_eq!(repo.commit_of("main").as_deref(), Some(tip.as_str()));
    assert_eq!(repo.branch_tip("main").as_deref(), Some(tip.as_str()));
    assert_eq!(repo.branch_tip("nope"), None);
    assert_eq!(repo.commit_of("nope"), None);
    assert_eq!(repo.commit_of("HEAD~1").unwrap().len(), 40);
}

#[test]
fn a_path_that_is_not_a_repository_is_an_error_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let said = Repo::new(dir.path()).worktrees().unwrap_err().to_string();
    assert!(said.contains("worktree list"), "{said}");
}

#[test]
fn the_default_branch_comes_from_origin_then_config_then_main_or_master() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    assert_eq!(repo.default_branch().as_deref(), Some("main"));
    git(&f.repo, &["branch", "develop"]);
    git(
        &f.repo,
        &["update-ref", "refs/remotes/origin/develop", "HEAD"],
    );
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/develop",
        ],
    );
    assert_eq!(repo.default_branch().as_deref(), Some("develop"));
    git(
        &f.repo,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    git(&f.repo, &["config", "init.defaultBranch", "develop"]);
    assert_eq!(repo.default_branch().as_deref(), Some("develop"));
    git(&f.repo, &["config", "init.defaultBranch", "absent"]);
    assert_eq!(repo.default_branch().as_deref(), Some("main"));
}

#[test]
fn a_worktree_is_added_detached_and_removed_through_git_with_branches_intact() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let dest = f.dir.path().join("slot/repo");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let base = repo.commit_of("main").unwrap();
    repo.add_worktree(&dest, &base).unwrap();
    let canonical = dest.canonicalize().unwrap();
    let trees = repo.worktrees().unwrap();
    assert_eq!(trees.len(), 2);
    assert_eq!(trees[1].path, canonical);
    assert_eq!(trees[1].branch, None, "a slot at rest is detached");
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]), base);
    // Dirty and untracked content does not stop the removal.
    std::fs::write(dest.join("stray.txt"), "x").unwrap();
    std::fs::write(dest.join("a.txt"), "changed").unwrap();
    git(&f.repo, &["branch", "keep-me"]);
    repo.remove_worktree(&dest).unwrap();
    assert!(!dest.exists());
    assert_eq!(repo.worktrees().unwrap().len(), 1);
    assert_eq!(
        git(&f.repo, &["branch", "--list", "keep-me"]).trim(),
        "keep-me"
    );
    assert!(!f.repo.join(".git/worktrees").exists(), "no stale entry");
}

#[test]
fn removing_a_worktree_whose_directory_is_gone_prunes_its_entry() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let dest = f.dir.path().join("slot/repo");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    repo.add_worktree(&dest, "main").unwrap();
    std::fs::remove_dir_all(&dest).unwrap();
    repo.remove_worktree(&dest).unwrap();
    assert_eq!(repo.worktrees().unwrap().len(), 1);
}

#[test]
fn a_base_that_is_no_commit_makes_no_worktree() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let dest = f.dir.path().join("slot/repo");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    assert!(repo.add_worktree(&dest, "no-such-commit").is_err());
    assert_eq!(repo.worktrees().unwrap().len(), 1);
}

#[test]
fn a_linked_worktree_finds_the_same_common_directory() {
    let f = fixture();
    let dest = f.dir.path().join("slot/repo");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    Repo::new(&f.repo).add_worktree(&dest, "main").unwrap();
    let main = Repo::new(&f.repo).common_dir().unwrap();
    assert_eq!(Repo::new(&dest).common_dir().unwrap(), main);
    assert!(main.ends_with(".git"));
}

#[test]
fn the_lock_is_exclusive_between_holders_and_free_again_after_a_drop() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let held = repo.lock().unwrap();
    let dir = repo.common_dir().unwrap();
    assert!(try_lock_path(&dir).unwrap().is_none(), "held: not free");
    drop(held);
    // A child forked by another thread holds a copy of the descriptor until it
    // execs, so the lock can free a few milliseconds after the drop.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while try_lock_path(&dir).unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "dropped: still not free"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("lock"))
            .count(),
        0,
        "the lock adds no file to the repository"
    );
}

#[test]
fn concurrent_worktree_additions_all_succeed_one_at_a_time() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let handles: Vec<_> = (0..8)
        .map(|n| {
            let repo = repo.clone();
            let dest = f.dir.path().join(format!("slots/slot-{n}/repo"));
            std::thread::spawn(move || {
                std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                repo.add_worktree(&dest, "main")
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    assert_eq!(repo.worktrees().unwrap().len(), 9);
    for n in 0..8 {
        repo.remove_worktree(&f.dir.path().join(format!("slots/slot-{n}/repo")))
            .unwrap();
    }
    assert_eq!(repo.worktrees().unwrap().len(), 1);
}

/// Not a test of its own: the child half of the cross-process lock test. It
/// does nothing unless `VIVI_LOCK_PROBE` names a path; then it exits 0 when
/// it can take the lock and 3 when another process holds it.
#[test]
fn lock_probe_child() {
    let Some(path) = std::env::var_os("VIVI_LOCK_PROBE") else {
        return;
    };
    let free = try_lock_path(Path::new(&path)).unwrap().is_some();
    std::process::exit(if free { 0 } else { 3 });
}

fn probe(path: &Path) -> i32 {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "pool::local_git::tests::lock_probe_child",
            "--test-threads=1",
        ])
        .env("VIVI_LOCK_PROBE", path)
        .output()
        .unwrap()
        .status
        .code()
        .unwrap()
}

#[test]
fn the_lock_holds_across_processes() {
    let f = fixture();
    let repo = Repo::new(&f.repo);
    let dir = repo.common_dir().unwrap();
    let held = repo.lock().unwrap();
    assert_eq!(probe(&dir), 3, "another process cannot take the lock");
    drop(held);
    assert_eq!(probe(&dir), 0, "it can once the holder lets go");
}

#[test]
fn only_a_name_under_a_configured_remote_is_a_remote_branch() {
    let f = fixture();
    git(&f.repo, &["remote", "add", "origin", "/nowhere"]);
    git(&f.repo, &["remote", "add", "up/stream", "/nowhere2"]);
    let repo = Repo::new(&f.repo);
    let named = |rev: &str| repo.remote_branch_of(rev);
    let pair = |r: &str, b: &str| Some((r.to_string(), b.to_string()));
    assert_eq!(named("origin/next"), pair("origin", "next"));
    assert_eq!(
        named("origin/pump/quarantine/q1"),
        pair("origin", "pump/quarantine/q1")
    );
    assert_eq!(named("up/stream/x"), pair("up/stream", "x"));
    for not in [
        "next",
        "origin",
        "origin/",
        "stranger/next",
        "origin/a:b",
        "origin/a~1",
        "origin/a^",
        "origin/a..b",
        "-origin/a",
        "0123456789abcdef0123456789abcdef01234567",
    ] {
        assert_eq!(named(not), None, "{not}");
    }
}
