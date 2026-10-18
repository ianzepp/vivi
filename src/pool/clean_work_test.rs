use super::*;
use crate::pool::clean::ActionKind;
use crate::pool::clean_rule::{read_mark, write_mark};
use crate::pool::local_fixture::{fixture, git};
use crate::pool::local_slots::{CHECKOUT, WORKTREE_OF_FILE};
use crate::pool::slot::META_BASE;
use std::fs;

fn marked_slot(root: &std::path::Path, source: &std::path::Path, base: &str) -> std::path::PathBuf {
    let dir = root.join("proj/lanes/slot-1");
    fs::create_dir_all(dir.join("marks")).unwrap();
    fs::write(dir.join("marks/vivi_pool"), "lanes\n").unwrap();
    write_mark(&dir, "vivi_pool_state", "ready").unwrap();
    write_mark(&dir, META_BASE, base).unwrap();
    fs::write(
        dir.join(WORKTREE_OF_FILE),
        format!("{}\n", source.display()),
    )
    .unwrap();
    attach_origin(source);
    dir
}

fn attach_origin(repo: &std::path::Path) {
    if crate::pool::local_git::Repo::new(repo)
        .remote_url("origin")
        .is_some()
    {
        return;
    }
    let bare = repo.parent().unwrap().join("origin.git");
    git(
        repo,
        &["clone", "-q", "--bare", ".", bare.to_str().unwrap()],
    );
    git(repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(repo, &["fetch", "-q", "origin"]);
}

#[test]
fn uncommitted_source_changes_block_recycle() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    let dir = marked_slot(tmp.path(), &f.repo, &base);
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &base)
        .unwrap();
    fs::write(dir.join(CHECKOUT).join("a.txt"), "local edit\n").unwrap();
    let blockers = worktree_blockers(&dir.join(CHECKOUT), "main", Some("build"));
    assert!(
        blockers.iter().any(|b| b.contains("uncommitted")),
        "{blockers:?}"
    );
    let (acts, skips) = recycle_lane(
        "lanes",
        "slot-1",
        &dir,
        "main",
        Some("build"),
        "main",
        false,
    )
    .unwrap();
    assert!(acts.is_empty());
    assert!(skips.iter().any(|s| s.reason.contains("uncommitted")));
    assert!(dir.join(CHECKOUT).join("a.txt").is_file());
}

#[test]
fn commits_not_in_the_merge_lane_block_recycle() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    let dir = marked_slot(tmp.path(), &f.repo, &base);
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &base)
        .unwrap();
    git(
        &dir.join(CHECKOUT),
        &["checkout", "-q", "-b", "unit/open-a1"],
    );
    fs::write(dir.join(CHECKOUT).join("a.txt"), "unique\n").unwrap();
    git(&dir.join(CHECKOUT), &["commit", "-q", "-am", "unique"]);
    git(&dir.join(CHECKOUT), &["checkout", "-q", "--detach"]);
    let blockers = worktree_blockers(&dir.join(CHECKOUT), "main", None);
    assert!(
        blockers.iter().any(|b| b.contains("not in main")),
        "{blockers:?}"
    );
}

#[test]
fn a_clean_merged_worktree_is_removed_and_restored_detached() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    let dir = marked_slot(tmp.path(), &f.repo, &base);
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &base)
        .unwrap();
    fs::create_dir_all(dir.join(CHECKOUT).join("build")).unwrap();
    fs::write(dir.join(CHECKOUT).join("build/out"), "warm\n").unwrap();
    let (acts, skips) = recycle_lane(
        "lanes",
        "slot-1",
        &dir,
        "main",
        Some("build"),
        "main",
        false,
    )
    .unwrap();
    assert!(skips.is_empty(), "{skips:?}");
    assert!(acts.iter().any(|a| a.kind == ActionKind::Worktree));
    assert!(dir.join(CHECKOUT).join(".git").exists());
    assert!(!dir.join(CHECKOUT).join("build/out").exists());
    let trees = Repo::new(&f.repo).worktrees().unwrap();
    assert!(
        trees.iter().any(|t| t.path == dir.join(CHECKOUT)),
        "{trees:?}"
    );
}

#[test]
fn dry_run_recycle_deletes_nothing() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    let dir = marked_slot(tmp.path(), &f.repo, &base);
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &base)
        .unwrap();
    let (acts, _) = recycle_lane("lanes", "slot-1", &dir, "main", None, "main", true).unwrap();
    assert!(acts.iter().any(|a| a.kind == ActionKind::Worktree));
    assert!(dir.join(CHECKOUT).join("a.txt").is_file());
}

#[test]
fn a_local_unit_branch_in_the_merge_lane_is_deleted() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["branch", "unit/done-a1"]);
    let dir = marked_slot(tmp.path(), &f.repo, &base);
    write_mark(&dir, "last_branch", "unit/done-a1").unwrap();
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &base)
        .unwrap();
    let (acts, skips) = recycle_lane("lanes", "slot-1", &dir, "main", None, "main", false).unwrap();
    assert!(skips.is_empty(), "{skips:?}");
    assert!(acts.iter().any(|a| a.kind == ActionKind::Branch));
    assert!(Repo::new(&f.repo).branch_tip("unit/done-a1").is_none());
    assert!(read_mark(&dir, "last_task").is_none());
}

#[test]
fn an_unmerged_unit_branch_is_reported_not_deleted() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    let dir = marked_slot(tmp.path(), &f.repo, &base);
    write_mark(&dir, "last_branch", "unit/open-a1").unwrap();
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &base)
        .unwrap();
    git(
        &dir.join(CHECKOUT),
        &["checkout", "-q", "-b", "unit/open-a1"],
    );
    fs::write(dir.join(CHECKOUT).join("a.txt"), "ahead\n").unwrap();
    git(&dir.join(CHECKOUT), &["commit", "-q", "-am", "ahead"]);
    git(&dir.join(CHECKOUT), &["checkout", "-q", "--detach"]);
    let (acts, skips) = recycle_lane("lanes", "slot-1", &dir, "main", None, "main", false).unwrap();
    assert!(acts.is_empty(), "{acts:?}");
    assert!(skips.iter().any(|s| s.reason.contains("not in main")));
    assert!(Repo::new(&f.repo).branch_tip("unit/open-a1").is_some());
    assert!(dir.join(CHECKOUT).join("a.txt").is_file());
}

#[test]
fn a_target_hash_cache_is_dropped_when_the_unit_is_in_the_merge_lane() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    let dir = marked_slot(tmp.path(), &f.repo, &base);
    let hash = "a".repeat(64);
    let cache = dir.join("targets").join(&hash).join(CHECKOUT);
    Repo::new(&f.repo).add_worktree(&cache, &base).unwrap();
    let (acts, skips) = recycle_lane("lanes", "slot-1", &dir, "main", None, "main", false).unwrap();
    assert!(skips.is_empty(), "{skips:?}");
    assert!(acts.iter().any(|a| a.kind == ActionKind::Target));
    assert!(!cache.exists());
    assert!(!dir.join("targets").join(&hash).exists());
}

#[test]
fn a_gone_worktree_entry_is_reported_as_an_orphan() {
    let f = fixture();
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("proj/lanes/slot-9/repo");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let base = git(&f.repo, &["rev-parse", "HEAD"]);
    Repo::new(&f.repo).add_worktree(&dest, &base).unwrap();
    fs::remove_dir_all(&dest).unwrap();
    let (acts, skips) =
        sweep_orphans("lanes", &f.repo, tmp.path(), Some("main"), |_| true, false).unwrap();
    assert!(skips.is_empty(), "{skips:?}");
    assert!(acts.iter().any(|a| a.kind == ActionKind::Orphan));
    let trees = Repo::new(&f.repo).worktrees().unwrap();
    assert!(
        trees.iter().all(|t| t.path != dest),
        "pruned entry still listed: {trees:?}"
    );
}
