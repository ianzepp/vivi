//! The provider through its trait, against directories and processes on this
//! machine.

use std::time::{Duration, Instant};

use super::*;
use crate::pool::slot::{META_STATE, PoolId, STATE_READY, slot_metadata, slot_state};

fn provider(root: &std::path::Path) -> LocalProvider {
    let config: PoolConfig = toml::from_str(&format!(
        "provider = \"local\"\nslots = 2\n[local]\nroot = \"{}\"\n",
        root.display()
    ))
    .unwrap();
    LocalProvider::new(&config)
}

fn spec(pool: &str, index: u32, boot: &str) -> CreateSpec {
    CreateSpec {
        name: format!("ms-{pool}-slot-{index}"),
        image: String::new(),
        metadata: slot_metadata(&PoolId::new("ms", "", pool), index, "main"),
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
