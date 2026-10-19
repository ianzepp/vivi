use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Barrier};

use super::*;
use crate::pool::local_fixture::{fixture, git};
use crate::pool::slot::{PoolId, slot_metadata};

fn spec(pool: &str, index: u32) -> CreateSpec {
    CreateSpec {
        name: format!("ms-{pool}-slot-{index}"),
        image: String::new(),
        metadata: slot_metadata(&PoolId::new("ms", "", pool), index, "main"),
        env: BTreeMap::from([("BASE".to_string(), "it's main".to_string())]),
        once_env: BTreeMap::from([("GH_TOKEN".to_string(), "sekrit".to_string())]),
        passthrough_env: Vec::new(),
        files: vec![
            ("/work/boot.sh".into(), "echo boot\n".into()),
            ("/work/source/README.md".into(), "# hi\n".into()),
        ],
        command: vec!["bash".into(), "/work/boot.sh".into()],
        build_dir: Some("target".into()),
        tmpfs: String::new(),
    }
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("lanes");
    (dir, root)
}

fn make(root: &Path, pool: &str, index: u32) -> (String, PathBuf) {
    let id = format!("ms/{pool}/slot-{index}");
    create(root, &id, &spec(pool, index)).unwrap();
    let dir = root.join(&id);
    (id, dir)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn ready(dir: &Path) {
    seal(dir, META_STATE, STATE_READY).unwrap();
}

#[test]
fn a_new_slot_holds_marks_environment_files_tools_and_a_private_secret() {
    let (_tmp, root) = root();
    let (id, dir) = make(&root, "lanes-a", 1);
    assert_eq!(id, "ms/lanes-a/slot-1");
    let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).unwrap();
    assert_eq!(read("marks/vivi_pool"), "lanes-a\n");
    assert_eq!(read("marks/vivi_pool_mailspace"), "ms\n");
    assert_eq!(read("marks/vivi_pool_state"), "seeding\n");
    assert!(!read("marks/vivi_pool_arch").trim().is_empty());
    assert_eq!(read("slot.env"), "BASE='it'\\''s main'\n");
    assert_eq!(read("boot.sh"), "echo boot\n");
    assert_eq!(read("source/README.md"), "# hi\n");
    assert!(read("once.env").contains("export GH_TOKEN='sekrit'"));
    assert!(
        !read("slot.env").contains("sekrit"),
        "no secret in the environment file"
    );
    assert!(read("created").trim().ends_with('Z'));
    assert_eq!(mode(&dir.join("once.env")), 0o600);
    assert_eq!(mode(&dir), 0o700, "private to the user");
    for tool in ["timeout", "sha256sum", "nproc"] {
        assert_eq!(mode(&dir.join("bin").join(tool)), 0o700, "{tool}");
    }
    assert!(root.join(".vivi-root").is_file());
}

#[test]
fn creating_over_an_existing_slot_is_refused_and_leaves_it_alone() {
    let (_tmp, root) = root();
    let (id, dir) = make(&root, "lanes-a", 1);
    std::fs::write(dir.join("keep"), "x").unwrap();
    let said = create(&root, &id, &spec("lanes-a", 1))
        .unwrap_err()
        .to_string();
    assert!(said.contains("slot directory exists"), "{said}");
    assert!(dir.join("keep").is_file());
}

#[test]
fn a_slot_value_with_a_line_break_is_refused_and_leaves_nothing_behind() {
    let (_tmp, root) = root();
    let mut bad = spec("lanes-a", 1);
    bad.metadata.insert("vivi_pool_base".into(), "a\nb".into());
    assert!(create(&root, "ms/lanes-a/slot-1", &bad).is_err());
    assert!(list(&root).unwrap().is_empty());
    let leftovers: Vec<_> = std::fs::read_dir(root.join("ms/lanes-a"))
        .unwrap()
        .flatten()
        .collect();
    assert!(leftovers.is_empty(), "no half-made slot: {leftovers:?}");
}

#[test]
fn slots_are_listed_from_their_directories_alone() {
    let (_tmp, root) = root();
    assert!(
        list(&root).unwrap().is_empty(),
        "no root yet is an empty list"
    );
    let (id, _) = make(&root, "lanes-a", 1);
    make(&root, "lanes-b", 2);
    std::fs::create_dir_all(root.join("ms/lanes-a/not-a-slot")).unwrap();
    std::fs::create_dir_all(root.join("ms/lanes-a/.partial-slot-9.1")).unwrap();
    let found = list(&root).unwrap();
    assert_eq!(found.len(), 2);
    let first = found.iter().find(|m| m.id == id).unwrap();
    assert_eq!(first.name, "ms-lanes-a-slot-1");
    assert_eq!(first.state, "stopped");
    assert_eq!(first.region, "local");
    assert_eq!(first.metadata["vivi_pool_slot"], "1");
    assert!(first.created_at.ends_with('Z'));
    assert!(!first.metadata.contains_key("vivi_pool_task"));
}

#[test]
fn two_mailspaces_with_one_pool_name_keep_separate_directories_and_marks() {
    let (_tmp, root) = root();
    make(&root, "lanes-a", 1);
    let mut other = spec("lanes-a", 1);
    other.metadata = slot_metadata(&PoolId::new("other", "", "lanes-a"), 1, "main");
    create(&root, "other/lanes-a/slot-1", &other).unwrap();
    let found = list(&root).unwrap();
    assert_eq!(found.len(), 2);
    let spaces: Vec<_> = found
        .iter()
        .map(|m| m.metadata["vivi_pool_mailspace"].clone())
        .collect();
    assert!(spaces.contains(&"ms".to_string()) && spaces.contains(&"other".to_string()));
}

#[test]
fn a_claim_marks_the_slot_with_task_and_time_and_unclaim_frees_it() {
    let (_tmp, root) = root();
    let (id, dir) = make(&root, "lanes-a", 1);
    ready(&dir);
    let hold = claim(&dir, "task-1", 1234).unwrap().unwrap();
    assert!(!hold.nonce.is_empty());
    let record = record_of(&dir, &id);
    assert_eq!(record.metadata["vivi_pool_task"], "task-1");
    assert_eq!(record.metadata["vivi_pool_claimed"], "1234");
    assert!(claim(&dir, "task-2", 1).unwrap().is_none(), "taken");
    unclaim(&dir).unwrap();
    unclaim(&dir).unwrap();
    assert!(!record_of(&dir, &id).metadata.contains_key("vivi_pool_task"));
    assert!(claim(&dir, "task-2", 1).unwrap().is_some());
}

#[test]
fn a_slot_that_is_not_ready_cannot_be_claimed_and_keeps_no_claim() {
    let (_tmp, root) = root();
    let (_, dir) = make(&root, "lanes-a", 1);
    assert!(claim(&dir, "task-1", 1).unwrap().is_none());
    assert!(!dir.join("claim").exists());
}

#[test]
fn claiming_a_slot_that_is_gone_is_an_error() {
    let (_tmp, root) = root();
    let said = claim(&root.join("ms/p/slot-1"), "t", 1)
        .unwrap_err()
        .to_string();
    assert!(said.contains("is gone"), "{said}");
}

#[test]
fn many_claimants_at_once_yield_exactly_one_winner() {
    let (_tmp, root) = root();
    let (_, dir) = make(&root, "lanes-a", 1);
    ready(&dir);
    let racers = 8;
    let gate = Arc::new(Barrier::new(racers));
    let handles: Vec<_> = (0..racers)
        .map(|n| {
            let (dir, gate) = (dir.clone(), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.wait();
                claim(&dir, &format!("task-{n}"), 1).unwrap()
            })
        })
        .collect();
    let winners = handles
        .into_iter()
        .filter_map(|h| h.join().unwrap())
        .count();
    assert_eq!(winners, 1);
}

#[test]
fn only_the_claim_holder_can_deliver_a_file_and_it_lands_whole_and_private() {
    let (_tmp, root) = root();
    let (_, dir) = make(&root, "lanes-a", 1);
    ready(&dir);
    let hold = claim(&dir, "task-1", 1).unwrap().unwrap();
    put(&dir, &hold, "/work/job.tgz", b"archive").unwrap();
    assert_eq!(std::fs::read(dir.join("job.tgz")).unwrap(), b"archive");
    assert_eq!(mode(&dir.join("job.tgz")), 0o600);
    assert!(!dir.join("job.partial").exists());
    let stranger = Hold {
        nonce: "wrong".into(),
    };
    let said = put(&dir, &stranger, "/work/job.tgz", b"x")
        .unwrap_err()
        .to_string();
    assert!(said.contains("not the claim holder"), "{said}");
    for bad in ["/etc/passwd", "/work/../x", "/work/", "job.tgz"] {
        assert!(put(&dir, &hold, bad, b"x").is_err(), "{bad}");
    }
}

#[test]
fn a_mark_name_is_plain_and_its_value_one_line() {
    let (_tmp, root) = root();
    let (_, dir) = make(&root, "lanes-a", 1);
    assert!(seal(&dir, "../x", "v").is_err());
    assert!(seal(&dir, "Bad", "v").is_err());
    assert!(seal(&dir, "vivi_pool_state", "a\nb").is_err());
    seal(&dir, "vivi_pool_state", "ready").unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("marks/vivi_pool_state")).unwrap(),
        "ready\n"
    );
}

#[test]
fn a_stopped_slot_is_read_in_place_with_the_paths_the_pool_spells() {
    let (_tmp, root) = root();
    let (_, dir) = make(&root, "lanes-a", 1);
    std::fs::create_dir_all(dir.join("job")).unwrap();
    std::fs::write(dir.join("job/result"), "job=1\n").unwrap();
    let files = read_files(&dir, &["/work/job/result", "/work/job/agent.out"]).unwrap();
    assert_eq!(files.len(), 1, "a file that is not there is left out");
    assert_eq!(files["/work/job/result"], b"job=1\n");
    assert!(read_files(&dir, &["/work/../etc/passwd"]).is_err());
}

fn refused(result: Result<(), VivariumError>) -> bool {
    result.is_err_and(|e| e.to_string().contains("refusing to delete"))
}

#[test]
fn destroy_removes_the_slot_its_pool_directory_and_a_root_it_made() {
    let (_tmp, root) = root();
    let (id, _) = make(&root, "lanes-a", 1);
    destroy(&root, &id).unwrap();
    assert!(!root.exists(), "the root Vivi made goes with its last slot");
    destroy(&root, &id).unwrap();
}

#[test]
fn destroy_leaves_a_root_it_did_not_make_and_other_pools_and_files_alone() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(&root).unwrap();
    let (id, _) = make(&root, "lanes-a", 1);
    let (_, other) = make(&root, "lanes-b", 1);
    std::fs::write(root.join("mine.txt"), "keep").unwrap();
    destroy(&root, &id).unwrap();
    assert!(!root.join("ms/lanes-a").exists());
    assert!(other.is_dir());
    assert!(root.join("mine.txt").is_file());
}

#[test]
fn destroy_refuses_ids_that_climb_out_of_the_root_or_are_no_slot_ids() {
    let (_tmp, root) = root();
    make(&root, "lanes-a", 1);
    for id in [
        "../x/y/slot-1",
        "ms/../..",
        "ms/lanes-a",
        "ms/lanes-a/slot-1/..",
        "/abs/p/slot-1",
        "ms/lanes-a/slot-x",
        "",
    ] {
        assert!(refused(destroy(&root, id)), "{id:?}");
    }
    assert!(root.join("ms/lanes-a/slot-1").is_dir());
}

#[test]
fn destroy_refuses_a_directory_without_vivis_marks_or_with_another_pools() {
    let (_tmp, root) = root();
    std::fs::create_dir_all(root.join("ms/mine/slot-1")).unwrap();
    std::fs::write(root.join("ms/mine/slot-1/precious"), "x").unwrap();
    assert!(refused(destroy(&root, "ms/mine/slot-1")));
    assert!(root.join("ms/mine/slot-1/precious").is_file());
    let (_, dir) = make(&root, "lanes-a", 1);
    std::fs::write(dir.join("marks/vivi_pool"), "other-pool\n").unwrap();
    assert!(refused(destroy(&root, "ms/lanes-a/slot-1")));
    std::fs::write(dir.join("marks/vivi_pool"), "lanes-a\n").unwrap();
    std::fs::write(dir.join("marks/vivi_pool_mailspace"), "elsewhere\n").unwrap();
    assert!(refused(destroy(&root, "ms/lanes-a/slot-1")));
    assert!(dir.is_dir());
}

#[test]
fn destroy_refuses_a_slot_that_is_a_link_and_never_follows_it() {
    let (_tmp, root) = root();
    let (_, real) = make(&root, "lanes-b", 1);
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.join("ms/lanes-a")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("ms/lanes-a/slot-1")).unwrap();
    std::fs::write(outside.path().join("precious"), "x").unwrap();
    assert!(refused(destroy(&root, "ms/lanes-a/slot-1")));
    assert!(outside.path().join("precious").is_file());
    std::os::unix::fs::symlink(&real, root.join("ms/lanes-a/slot-2")).unwrap();
    assert!(refused(destroy(&root, "ms/lanes-a/slot-2")));
    assert!(real.is_dir());
}

#[test]
fn destroy_refuses_the_filesystem_root_however_the_slot_looks() {
    assert!(refused(destroy(Path::new("/"), "ms/lanes-a/slot-1")));
}

/// A worktree slot: the spec names the repository and the base commit.
fn worktree_spec(repo: &Path, base: &str) -> CreateSpec {
    let mut spec = spec("wt", 1);
    spec.env = BTreeMap::from([
        ("WORKTREE_OF".to_string(), repo.display().to_string()),
        ("BASE".to_string(), base.to_string()),
    ]);
    spec
}

#[test]
fn a_worktree_slot_gets_a_detached_checkout_and_destroy_removes_it_through_git() {
    let f = fixture();
    let (_tmp, root) = root();
    let base = git(&f.repo, &["rev-parse", "main"]);
    git(&f.repo, &["branch", "keep-me"]);
    create(&root, "ms/wt/slot-1", &worktree_spec(&f.repo, &base)).unwrap();
    let checkout = root.join("ms/wt/slot-1").join(CHECKOUT);
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), base);
    assert_eq!(
        git(&checkout, &["branch", "--show-current"]),
        "",
        "detached"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("ms/wt/slot-1").join(WORKTREE_OF_FILE)).unwrap(),
        f.repo.display().to_string()
    );
    assert_eq!(git(&f.repo, &["worktree", "list"]).lines().count(), 2);
    destroy(&root, "ms/wt/slot-1").unwrap();
    assert_eq!(git(&f.repo, &["worktree", "list"]).lines().count(), 1);
    assert!(
        !f.repo.join(".git/worktrees").exists(),
        "no stale worktree entry"
    );
    assert_eq!(
        git(&f.repo, &["branch", "--list", "keep-me", "main"])
            .lines()
            .count(),
        2,
        "every branch is intact"
    );
    assert!(!root.exists());
}

#[test]
fn a_worktree_slot_whose_repository_refuses_the_base_is_not_left_behind() {
    let f = fixture();
    let (_tmp, root) = root();
    let said = create(
        &root,
        "ms/wt/slot-1",
        &worktree_spec(&f.repo, "no-such-commit"),
    )
    .unwrap_err()
    .to_string();
    assert!(said.contains("worktree"), "{said}");
    assert!(list(&root).unwrap().is_empty());
    assert!(!root.join("ms/wt/slot-1").exists());
    assert_eq!(git(&f.repo, &["worktree", "list"]).lines().count(), 1);
}

#[test]
fn destroy_still_removes_a_worktree_slot_whose_checkout_was_deleted_by_hand() {
    let f = fixture();
    let (_tmp, root) = root();
    create(&root, "ms/wt/slot-1", &worktree_spec(&f.repo, "main")).unwrap();
    std::fs::remove_dir_all(root.join("ms/wt/slot-1").join(CHECKOUT)).unwrap();
    destroy(&root, "ms/wt/slot-1").unwrap();
    assert!(
        !f.repo.join(".git/worktrees").exists(),
        "the entry is pruned"
    );
}

#[test]
fn a_slot_at_rest_is_put_back_on_a_detached_head_and_its_branch_is_released() {
    let f = fixture();
    let (_tmp, root) = root();
    create(&root, "ms/wt/slot-1", &worktree_spec(&f.repo, "main")).unwrap();
    let dir = root.join("ms/wt/slot-1");
    let checkout = dir.join(CHECKOUT);
    git(&checkout, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(checkout.join("work.txt"), "unfinished").unwrap();
    assert_eq!(git(&checkout, &["branch", "--show-current"]), "topic");
    detach_if_idle(&dir);
    assert_eq!(git(&checkout, &["branch", "--show-current"]), "");
    assert_eq!(
        git(&f.repo, &["branch", "--list", "topic"]).trim(),
        "topic",
        "the branch is intact"
    );
    assert!(
        checkout.join("work.txt").is_file(),
        "the working tree is not touched"
    );
    // Free now: the primary checkout can take the branch.
    git(&f.repo, &["checkout", "-q", "topic"]);
    git(&f.repo, &["checkout", "-q", "main"]);
}

#[test]
fn a_slot_without_a_worktree_is_left_alone_by_the_detach() {
    let (_tmp, root) = root();
    let (_, dir) = make(&root, "lanes-a", 1);
    detach_if_idle(&dir);
    assert!(!dir.join(CHECKOUT).exists());
}
