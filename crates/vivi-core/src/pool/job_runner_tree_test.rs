use super::*;
use crate::pool::job::sh_quote;
use std::os::unix::fs::{MetadataExt, symlink};

fn settings(tree: &Path) -> String {
    format!("SLOT_DIR={}\n", sh_quote(&tree.to_string_lossy()))
}

fn tree(rig: &Rig) -> PathBuf {
    rig.work
        .path()
        .canonicalize()
        .unwrap()
        .join("trees/owner/repo/slot-1")
}

fn run(rig: &Rig, tree: &Path, harness: &str, extra: &str) -> JobResult {
    rig.run_in_context_logged(
        harness,
        "",
        &[],
        &rig.f.repo,
        Some(("alias", &"a".repeat(64))),
        &(settings(tree) + extra),
    )
    .0
}

#[test]
fn configured_tree_reuses_checkout_git_metadata_mtimes_and_ignored_output_across_two_jobs() {
    let rig = Rig::new();
    let tree = rig
        .work
        .path()
        .canonicalize()
        .unwrap()
        .join("tree roots ' quoted/owner/repo/slot-1");
    let first = run(&rig, &tree, "mkdir -p build; echo warm > build/cache", "");
    assert_eq!(first.push(), "read_only_unchanged", "{:?}", first.fields);
    let repo = tree.clone();
    let before = std::fs::metadata(repo.join("a.txt")).unwrap();
    let gitdir = std::fs::metadata(repo.join(".git")).unwrap().ino();
    // Tracked and loose leftovers from a prior process must be cleaned.
    std::fs::write(repo.join("loose.txt"), "leftover").unwrap();
    let second = run(
        &rig,
        &tree,
        "test ! -e loose.txt && grep warm build/cache && grep one a.txt",
        "",
    );
    assert_eq!(second.get("agent_rc"), "0", "{:?}", second.fields);
    assert_eq!(second.push(), "read_only_unchanged");
    let after = std::fs::metadata(repo.join("a.txt")).unwrap();
    assert_eq!(before.ino(), after.ino(), "checkout stays in place");
    assert_eq!(
        before.modified().unwrap(),
        after.modified().unwrap(),
        "unchanged source mtime stays warm"
    );
    assert_eq!(gitdir, std::fs::metadata(repo.join(".git")).unwrap().ino());
    assert_eq!(
        std::fs::read_to_string(rig.work.path().join("source")).unwrap(),
        format!("file://{}", rig.f.repo.display())
    );
    assert!(rig.work.path().join("prepared").is_file());
    assert!(!rig.work.path().join("targets").exists());
}

#[test]
fn configured_tree_refuses_symlinks_at_checkout_cache_and_ancestor_paths() {
    for level in [
        "trees",
        "trees/owner/repo/slot-1",
        "trees/owner/repo/slot-1/.git",
    ] {
        let rig = Rig::new();
        let external = tempfile::tempdir().unwrap();
        let path = rig.work.path().join(level);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(external.path(), &path).unwrap();
        let result = run(&rig, &tree(&rig), "touch should-not-run", "");
        assert_eq!(
            result.push(),
            "target_cache_invalid",
            "{level}: {:?}",
            result.fields
        );
        assert!(std::fs::read_dir(external.path()).unwrap().next().is_none());
    }
}

#[test]
fn configured_tree_refuses_a_different_source_and_relative_paths() {
    let rig = Rig::new();
    let tree = tree(&rig);
    assert_eq!(run(&rig, &tree, "true", "").push(), "read_only_unchanged");
    std::fs::write(
        rig.work.path().join("source"),
        "git@elsewhere:other/repo.git",
    )
    .unwrap();
    assert_eq!(
        run(&rig, &tree, "true", "").push(),
        "target_source_mismatch"
    );
    assert_eq!(
        run(&rig, Path::new("relative/tree"), "true", "").push(),
        "target_cache_invalid"
    );
    assert_eq!(
        run(&rig, Path::new("/"), "true", "").push(),
        "target_cache_invalid"
    );
}

#[test]
fn configured_tree_supports_a_prepared_local_mirror_worktree_and_checks_its_record() {
    let rig = Rig::new();
    let tree = tree(&rig);
    std::fs::create_dir_all(tree.parent().unwrap()).unwrap();
    git(
        &rig.f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            tree.to_str().unwrap(),
            &rig.start,
        ],
    );
    std::fs::write(
        rig.work.path().join("worktree_of"),
        rig.f.repo.to_str().unwrap(),
    )
    .unwrap();
    let extra = format!(
        "TARGET_LOCAL_SOURCE={}\nLOCAL_ACTION=detached\nLOCAL_START={}\n",
        sh_quote(rig.f.repo.to_str().unwrap()),
        sh_quote(&rig.start)
    );
    assert_eq!(
        run(
            &rig,
            &tree,
            "mkdir -p build; echo mirror > build/cache",
            &extra
        )
        .push(),
        "read_only_unchanged"
    );
    assert_eq!(
        run(&rig, &tree, "grep mirror build/cache", &extra).get("agent_rc"),
        "0"
    );
    std::fs::write(rig.work.path().join("worktree_of"), "/different/mirror").unwrap();
    assert_eq!(
        run(&rig, &tree, "true", &extra).push(),
        "target_source_mismatch"
    );
}

/// The fleet layout: a host keeps one full bare mirror per repository and each
/// of the repository's slots is a worktree of it, so every job of every slot
/// fetches into that one mirror. A shallow fetch there (the pool's DEPTH is 1
/// here) would mark the shared mirror shallow for all slots, and slots
/// starting together meet each other's locks on the shared repository.
#[test]
fn a_worktree_slot_of_a_shared_mirror_fetches_full_history_and_waits_out_a_held_lock() {
    let rig = Rig::new();
    let mirror = rig.work.path().canonicalize().unwrap().join("mirror.git");
    git(
        rig.work.path(),
        &[
            "clone",
            "-q",
            "--bare",
            &rig.f.repo.display().to_string(),
            mirror.to_str().unwrap(),
        ],
    );
    let tree = tree(&rig);
    std::fs::create_dir_all(tree.parent().unwrap()).unwrap();
    git(
        &mirror,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            tree.to_str().unwrap(),
            &rig.start,
        ],
    );
    // Another slot's job holds the mirror's config lock for a moment.
    let lock = mirror.join("config.lock");
    std::fs::write(&lock, "").unwrap();
    let release = {
        let lock = lock.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(6));
            std::fs::remove_file(lock).unwrap();
        })
    };
    let result = run(&rig, &tree, "grep two a.txt", "");
    release.join().unwrap();
    assert_eq!(result.get("agent_rc"), "0", "{:?}", result.fields);
    assert_eq!(result.push(), "read_only_unchanged");
    assert_eq!(
        git(&mirror, &["rev-parse", "--is-shallow-repository"]),
        "false",
        "a slot's fetch leaves the shared mirror's history whole"
    );
    let fetch_head = git(
        &tree,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "FETCH_HEAD",
        ],
    );
    assert!(
        !Path::new(&fetch_head).exists(),
        "FETCH_HEAD is removed through its Git path: in a worktree `.git` is a file"
    );
}

#[test]
fn unset_tree_keeps_the_existing_keyed_cache_location() {
    let rig = Rig::new();
    let key = "b".repeat(64);
    let result = rig.run_in_context(
        "mkdir -p build; echo warm > build/cache",
        "",
        &[],
        &rig.f.repo,
        Some(("repo", &key)),
    );
    assert_eq!(result.push(), "read_only_unchanged");
    let cache = rig.work.path().join("targets").join(key);
    assert!(cache.join("repo/build/cache").is_file());
    assert!(cache.join("source").is_file());
    assert!(!rig.work.path().join("trees").exists());
}

/// A declared slot's runner state is in `<slot>/.vivi`, inside the slot's Git
/// worktree. Its temp dir must not be: tools and tests that assume a temp dir
/// is outside any repository, or that skip `.vivi` paths, misbehave there.
#[test]
fn a_declared_slot_gets_an_empty_temp_dir_beside_the_slot_outside_its_worktree() {
    let rig = Rig::new();
    let tree = tree(&rig);
    let expected = tree.parent().unwrap().join(".slot-1.tmp");
    std::fs::create_dir_all(&expected).unwrap();
    std::fs::write(expected.join("stale"), "from the previous job").unwrap();
    let harness = format!(
        "test \"$TMPDIR\" = {}; test -d \"$TMPDIR\"; test ! -e \"$TMPDIR/stale\"; ! git -C \"$TMPDIR\" rev-parse --git-dir 2>/dev/null",
        sh_quote(&expected.to_string_lossy())
    );
    let result = run(&rig, &tree, &harness, "");
    assert_eq!(result.get("agent_rc"), "0", "{:?}", result.fields);
}
