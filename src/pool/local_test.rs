//! The provider through its trait, against directories and processes on this
//! machine.

use std::time::{Duration, Instant};

use super::*;
use crate::pool::slot::{META_STATE, PoolId, STATE_READY, slot_metadata, slot_state};

fn provider(root: &std::path::Path) -> LocalProvider {
    provider_for(
        root.parent().unwrap(),
        root.to_str().unwrap(),
        "ms",
        "lanes-a",
    )
}

fn provider_for(
    project: &std::path::Path,
    root: &str,
    mailspace: &str,
    pool: &str,
) -> LocalProvider {
    let config: PoolConfig = toml::from_str(&format!(
        "provider = \"local\"\nslots = 2\n[local]\nroot = \"{root}\"\n"
    ))
    .unwrap();
    LocalProvider::new(
        &config,
        &super::super::runpod::HostContext {
            dir: project.join(".vivi"),
            mailspace: mailspace.into(),
            pool: pool.into(),
            pools: vec![pool.into()],
        },
    )
}

fn spec(pool: &str, index: u32, boot: &str) -> CreateSpec {
    spec_for("ms", pool, index, boot)
}

fn spec_for(mailspace: &str, pool: &str, index: u32, boot: &str) -> CreateSpec {
    CreateSpec {
        name: format!("{mailspace}-{pool}-slot-{index}"),
        image: String::new(),
        metadata: slot_metadata(&PoolId::new(mailspace, "", pool), index, "main"),
        env: BTreeMap::new(),
        once_env: BTreeMap::new(),
        passthrough_env: vec!["IGNORED".into()],
        files: vec![("/work/boot.sh".into(), boot.into())],
        command: vec!["bash".into(), "/work/boot.sh".into()],
        build_dir: None,
        tmpfs: String::new(),
    }
}

fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("lanes");
    (dir, root)
}

#[test]
fn refuse_foreign_worktree_names_both_repositories_and_the_fix() {
    let configured = tempfile::tempdir().unwrap();
    let recorded = tempfile::tempdir().unwrap();
    let slot = tempfile::tempdir().unwrap();
    let marker = slot.path().join(slots::WORKTREE_OF_FILE);
    std::fs::write(&marker, format!("{}\n", recorded.path().display())).unwrap();
    let error = refuse_foreign_worktree("e2e/wt/slot-1", slot.path(), configured.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("worktree of"), "{error}");
    assert!(
        error.contains(&configured.path().display().to_string()),
        "{error}"
    );
    assert!(
        error.contains(&recorded.path().display().to_string()),
        "{error}"
    );
    assert!(error.contains("vivi pool destroy e2e/wt/slot-1"), "{error}");

    std::fs::write(&marker, format!("{}\n", configured.path().display())).unwrap();
    assert!(refuse_foreign_worktree("e2e/wt/slot-1", slot.path(), configured.path()).is_ok());
    std::fs::remove_file(marker).unwrap();
    assert!(refuse_foreign_worktree("e2e/wt/slot-1", slot.path(), configured.path()).is_ok());
}

fn plant_slot(root: &std::path::Path, id: &str, mailspace: &str, pool: &str) {
    let dir = root.join(id);
    std::fs::create_dir_all(dir.join("marks")).unwrap();
    std::fs::write(dir.join("marks/vivi_pool"), format!("{pool}\n")).unwrap();
    std::fs::write(
        dir.join("marks/vivi_pool_mailspace"),
        format!("{mailspace}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("marks/vivi_pool_state"), "ready\n").unwrap();
}

#[test]
fn the_provider_says_what_a_directory_on_this_machine_is() {
    let (_tmp, root) = root();
    let p = provider(&root);
    assert!(!p.supplies_build_mount());
    assert!(p.build_persists());
    assert!(p.runs_unprivileged());
    assert_eq!(p.ensure_home().unwrap(), None);
    assert!(p.live_test_notice().contains(&root.display().to_string()));
    assert!(p.destroy_command("ms/p/slot-1").contains("rm -rf"));
}

#[test]
fn project_relative_root_lays_out_a_two_part_slot_id() {
    let project = tempfile::tempdir().unwrap();
    let provider = provider_for(project.path(), "pool", "space", "here");
    let id = provider
        .create(&spec_for("space", "here", 1, "exit 0\n"))
        .unwrap();
    assert_eq!(id, "here/slot-1");
    assert!(project.path().join("pool/here/slot-1").is_dir());
    assert_eq!(provider.list().unwrap()[0].id, id);
}

#[test]
fn absolute_root_keeps_the_mailspace_id_segment() {
    let project = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let provider = provider_for(
        project.path(),
        root.path().to_str().unwrap(),
        "space",
        "here",
    );
    let id = provider
        .create(&spec_for("space", "here", 1, "exit 0\n"))
        .unwrap();
    assert_eq!(id, "space/here/slot-1");
    assert!(root.path().join(&id).is_dir());
    assert_eq!(provider.list().unwrap()[0].id, id);
}

#[test]
fn listing_never_leaves_the_selected_pool_directory() {
    let project = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let provider = provider_for(
        project.path(),
        root.path().to_str().unwrap(),
        "space-id",
        "here",
    );
    provider
        .create(&spec_for("space-id", "here", 1, "exit 0\n"))
        .unwrap();
    plant_slot(
        root.path(),
        "space-id/here-fetch/slot-1",
        "space-id",
        "here-fetch",
    );
    plant_slot(root.path(), "other-id/here/slot-1", "other-id", "here");
    assert_eq!(
        provider
            .list()
            .unwrap()
            .iter()
            .map(|machine| machine.id.as_str())
            .collect::<Vec<_>>(),
        ["space-id/here/slot-1"]
    );
}

#[test]
fn a_created_slot_runs_its_boot_to_its_end_and_reads_stopped_with_an_exit_code() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let id = p
        .create(&spec("lanes-a", 1, "echo booted > booted.txt\nexit 3\n"))
        .unwrap();
    assert_eq!(id, "ms/lanes-a/slot-1");
    until("the boot to end", || {
        p.machine(&id).unwrap().state == "stopped"
    });
    let record = p.machine(&id).unwrap();
    assert_eq!(record.exit_code, Some(3));
    assert_eq!(record.region, "local");
    let read = p
        .read_stopped(&id, &["/work/booted.txt", "/work/absent"])
        .unwrap()
        .unwrap();
    assert_eq!(read["/work/booted.txt"], b"booted\n");
    assert_eq!(read.len(), 1);
    assert_eq!(p.list().unwrap().len(), 1);
}

#[test]
fn start_and_stop_are_one_running_process_tree_and_nothing_else() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let id = p.create(&spec("lanes-a", 1, "sleep 60\n")).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "started");
    p.stop(&id).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "stopped");
    assert_eq!(
        p.machine(&id).unwrap().exit_code,
        None,
        "a stop leaves no exit code"
    );
    p.start(&id).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "started");
    p.destroy(&id).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "destroyed");
    assert!(!root.exists());
}

#[test]
fn exec_runs_in_the_slot_with_work_set_and_returns_the_scripts_own_exit_code() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let id = p.create(&spec("lanes-a", 1, "exit 0\n")).unwrap();
    let out = p
        .exec(&id, "pwd -P; echo \"$WORK\"; echo oops >&2; exit 4", 5)
        .unwrap();
    let dir = root.join(&id);
    assert_eq!(out.stdout, format!("{0}\n{0}\n", dir.display()));
    assert_eq!(out.stderr.trim(), "oops");
    assert_eq!(out.exit_code, 4);
}

#[test]
fn a_slot_is_claimed_once_sealed_ready_and_unclaimed_to_be_free_again() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let id = p.create(&spec("lanes-a", 1, "exit 0\n")).unwrap();
    until("the boot to end", || {
        p.machine(&id).unwrap().state == "stopped"
    });
    assert!(
        p.claim(&id, "task-1", 1).unwrap().is_none(),
        "not ready yet"
    );
    p.seal(&id, &["GH_TOKEN"], META_STATE, STATE_READY).unwrap();
    assert_eq!(slot_state(&p.machine(&id).unwrap()), "free");
    let hold = p.claim(&id, "task-1", 7).unwrap().unwrap();
    assert!(p.claim(&id, "task-2", 8).unwrap().is_none());
    p.put_file(&id, &hold, "/work/job.tgz", b"x").unwrap();
    p.release(&id, &hold).unwrap();
    assert_eq!(slot_state(&p.machine(&id).unwrap()), "finished-uncollected");
    p.unclaim(&id).unwrap();
    assert_eq!(slot_state(&p.machine(&id).unwrap()), "free");
}

#[test]
fn a_slot_that_is_not_there_reads_as_gone_and_a_bad_id_never_reaches_the_disk() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let gone = p.machine("ms/lanes-a/slot-9").unwrap();
    assert_eq!(gone.state, "destroyed");
    for bad in ["../x", "ms/lanes-a", "/etc", "ms/lanes-a/slot-1/../.."] {
        assert!(p.machine(bad).is_err(), "{bad}");
        assert!(p.start(bad).is_err(), "{bad}");
        assert!(p.destroy(bad).is_err(), "{bad}");
    }
    assert!(p.start("ms/lanes-a/slot-9").is_err());
}

#[test]
fn harness_names_come_from_the_environment_of_this_process_and_only_names_are_reported() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let names = vec![
        "PATH".to_string(),
        "VIVI_TEST_SURELY_UNSET_NAME".to_string(),
    ];
    assert_eq!(
        p.harness_env_gaps(&names).unwrap(),
        vec!["VIVI_TEST_SURELY_UNSET_NAME"]
    );
    assert_eq!(
        p.env_gaps_at_dispatch(&names).unwrap(),
        vec!["VIVI_TEST_SURELY_UNSET_NAME"]
    );
    let hint = p.env_gap_hint(&names[1..]);
    assert!(
        hint.contains("VIVI_TEST_SURELY_UNSET_NAME") && hint.contains("no value is stored"),
        "{hint}"
    );
}

#[test]
fn creating_a_slot_whose_boot_cannot_start_removes_it_again() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let mut bad = spec("lanes-a", 1, "exit 0\n");
    bad.metadata.remove("vivi_pool_slot");
    assert!(
        p.create(&bad)
            .unwrap_err()
            .to_string()
            .contains("lacks vivi_pool_slot")
    );
    assert!(p.list().unwrap().is_empty());
}
