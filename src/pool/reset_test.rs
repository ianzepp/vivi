use super::*;
use crate::mailspace::Mailspace;
use crate::pool::clean_rule::{read_mark, write_mark};
use crate::pool::local_fixture::{fixture, git};
use crate::pool::local_slots::{CHECKOUT, WORKTREE_OF_FILE};
use crate::pool::slot::STATE_READY;
use crate::pump::state::{Attempt, Status, Unit};
use std::fs;

fn attach_origin(repo: &Path) -> std::path::PathBuf {
    let bare = repo.parent().unwrap().join("origin.git");
    git(
        repo,
        &["clone", "-q", "--bare", ".", bare.to_str().unwrap()],
    );
    git(repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(repo, &["fetch", "-q", "origin"]);
    bare
}

fn free_slot(root: &Path, source: &Path) -> std::path::PathBuf {
    let dir = root.join("proj/lanes/slot-1");
    fs::create_dir_all(dir.join("marks")).unwrap();
    fs::write(dir.join("marks/vivi_pool"), "lanes\n").unwrap();
    write_mark(&dir, "vivi_pool_state", STATE_READY).unwrap();
    fs::write(
        dir.join(WORKTREE_OF_FILE),
        format!("{}\n", source.display()),
    )
    .unwrap();
    dir
}

fn advance_origin(origin: &Path, msg: &str) -> String {
    let tmp = origin.parent().unwrap().join(format!("push-{msg}"));
    git(
        origin,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            tmp.to_str().unwrap(),
        ],
    );
    git(&tmp, &["config", "user.name", "T"]);
    git(&tmp, &["config", "user.email", "t@localhost"]);
    fs::write(tmp.join("z.txt"), format!("{msg}\n")).unwrap();
    git(&tmp, &["add", "-A"]);
    git(&tmp, &["commit", "-q", "-m", msg]);
    git(&tmp, &["push", "-q", "origin", "HEAD:main"]);
    git(&tmp, &["rev-parse", "HEAD"])
}

fn pump_space(main: &str) -> (tempfile::TempDir, Mailspace) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(
        dir.path().join(".vivi/mailspace.toml"),
        format!("name = \"t\"\n\n[pump]\nbranch = \"next\"\nmain = \"{main}\"\n"),
    )
    .unwrap();
    let space = Mailspace::discover(Some(dir.path())).unwrap();
    (dir, space)
}

#[test]
fn default_reset_follows_advanced_origin_main() {
    let f = fixture();
    let origin = attach_origin(&f.repo);
    let tmp = tempfile::tempdir().unwrap();
    let dir = free_slot(tmp.path(), &f.repo);
    let first = git(&f.repo, &["rev-parse", "HEAD"]);
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &first)
        .unwrap();
    let out = reset_local(&dir, "main", "default-main", false).unwrap();
    assert_eq!(out.sha, first);
    assert_eq!(out.base_ref, "main");
    let later = advance_origin(&origin, "third");
    assert_ne!(later, first);
    let out = reset_local(&dir, "main", "default-main", false).unwrap();
    assert_eq!(out.sha, later);
    assert_eq!(
        Repo::new(&dir.join(CHECKOUT)).commit_of("HEAD").unwrap(),
        later
    );
    assert_eq!(read_mark(&dir, MARK_BASE_REF).as_deref(), Some("main"));
    assert_eq!(
        read_mark(&dir, MARK_BASE_SHA).as_deref(),
        Some(later.as_str())
    );
}

#[test]
fn reset_can_override_to_next() {
    let f = fixture();
    attach_origin(&f.repo);
    git(&f.repo, &["branch", "next"]);
    git(&f.repo, &["push", "-q", "origin", "next"]);
    let tmp = tempfile::tempdir().unwrap();
    let dir = free_slot(tmp.path(), &f.repo);
    let head = git(&f.repo, &["rev-parse", "HEAD"]);
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &head)
        .unwrap();
    let out = reset_local(&dir, "next", "unlanded-deps", false).unwrap();
    assert_eq!(out.base_ref, "next");
    assert_eq!(read_mark(&dir, MARK_BASE_REF).as_deref(), Some("next"));
}

#[test]
fn reset_can_override_to_a_work_branch() {
    let f = fixture();
    attach_origin(&f.repo);
    git(&f.repo, &["checkout", "-q", "-b", "unit/fix-a2"]);
    fs::write(f.repo.join("a.txt"), "fix\n").unwrap();
    git(&f.repo, &["commit", "-q", "-am", "fix"]);
    git(&f.repo, &["push", "-q", "origin", "unit/fix-a2"]);
    let tip = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["checkout", "-q", "main"]);
    let tmp = tempfile::tempdir().unwrap();
    let dir = free_slot(tmp.path(), &f.repo);
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), "main")
        .unwrap();
    let out = reset_local(&dir, "unit/fix-a2", "prior-attempt", false).unwrap();
    assert_eq!(out.sha, tip);
    assert_eq!(out.base_ref, "unit/fix-a2");
}

#[test]
fn a_missing_origin_branch_fails_and_leaves_the_slot() {
    let f = fixture();
    attach_origin(&f.repo);
    let tmp = tempfile::tempdir().unwrap();
    let dir = free_slot(tmp.path(), &f.repo);
    let head = git(&f.repo, &["rev-parse", "HEAD"]);
    Repo::new(&f.repo)
        .add_worktree(&dir.join(CHECKOUT), &head)
        .unwrap();
    let err = reset_local(&dir, "no-such-branch", "requested", false).unwrap_err();
    assert!(err.to_string().contains("no-such-branch"), "{err}");
    assert!(dir.join(CHECKOUT).join("a.txt").is_file());
    assert!(read_mark(&dir, MARK_BASE_REF).is_none());
}

#[test]
fn choose_base_defaults_to_pump_main_on_recycle() {
    let (_tmp, space) = pump_space("trunk");
    let unit = Unit {
        status: Status::Merged,
        after: vec!["dep".into()],
        history: vec![Attempt {
            branch: Some("unit/old-a1".into()),
            ..Attempt::default()
        }],
        ..Unit::default()
    };
    let chosen = choose_base(&space, None, Some(&unit), ResetKind::Recycle);
    assert_eq!(chosen.name, "trunk");
    assert_eq!(chosen.why, "default-main");
}

#[test]
fn choose_base_ignores_an_unfinished_dependency() {
    let (_tmp, space) = pump_space("main");
    let bare = Unit {
        after: vec!["dep".into()],
        ..Unit::default()
    };
    let chosen = choose_base(&space, None, Some(&bare), ResetKind::Dispatch);
    assert_eq!(chosen.name, "main");
    assert_eq!(chosen.why, "default-main");
    let prior = Unit {
        after: vec!["dep".into()],
        history: vec![Attempt {
            branch: Some("unit/old-a1".into()),
            ..Attempt::default()
        }],
        ..Unit::default()
    };
    let chosen = choose_base(&space, None, Some(&prior), ResetKind::Dispatch);
    assert_eq!(chosen.name, "unit/old-a1");
    assert_eq!(chosen.why, "prior-attempt");
}

#[test]
fn choose_base_uses_a_prior_attempt_branch() {
    let (_tmp, space) = pump_space("main");
    let unit = Unit {
        history: vec![Attempt {
            branch: Some("unit/u1-a1".into()),
            ..Attempt::default()
        }],
        ..Unit::default()
    };
    let chosen = choose_base(&space, None, Some(&unit), ResetKind::Dispatch);
    assert_eq!(chosen.name, "unit/u1-a1");
    assert_eq!(chosen.why, "prior-attempt");
}

#[test]
fn remote_script_names_fetch_and_checkout() {
    let script = remote_script("next");
    assert!(script.contains("git -C"), "{script}");
    assert!(script.contains("fetch"), "{script}");
    assert!(script.contains("origin"), "{script}");
    assert!(script.contains("next"), "{script}");
    assert!(script.contains("base_ref"), "{script}");
}
