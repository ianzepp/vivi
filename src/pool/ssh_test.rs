//! The provider against the real remote script, run on this machine instead
//! of over SSH: `Shell` hands the remote command line to `sh -c` with a
//! throwaway home directory, which is exactly what a login shell on the host
//! would do with it. What needs a Linux host (processes, `/proc`) is covered
//! by the live test `tests/pool_ssh_live.rs`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use super::*;
use crate::pool::docker_cli::finish;
use crate::pool::slot::{
    META_CLAIMED, META_STATE, META_TASK, PoolId, STATE_READY, slot_metadata, slot_state,
};
use crate::pool::ssh_script::command_line;

/// Runs a remote command line locally, with `home` as the login home.
struct Shell {
    home: PathBuf,
}

impl Runner for Shell {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let child = Command::new("sh")
            .arg("-c")
            .arg(&args[0])
            .env("HOME", &self.home)
            .env("COPYFILE_DISABLE", "1")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VivariumError::Message(e.to_string()))?;
        finish(child, stdin, timeout, "sh")
    }
}

fn config(root: &str) -> PoolConfig {
    toml::from_str(&format!(
        "repo = \"o/r\"\nslots = 2\nbuild_dir = \"target\"\nprovider = \"ssh\"\n[ssh]\nhost = \"box\"\nroot = \"{root}\"\n[github]\napp_id = 1\nprivate_key = \"k.pem\"\n"
    ))
    .unwrap()
}

fn provider(home: &Path) -> SshProvider {
    let shell = Shell {
        home: home.to_path_buf(),
    };
    SshProvider::with_runner(&config("~/lanes"), Box::new(shell))
}

fn spec(pool: &str, index: u32) -> CreateSpec {
    CreateSpec {
        name: format!("ms-{pool}-slot-{index}"),
        image: String::new(),
        metadata: slot_metadata(&PoolId::new("ms", pool), index, "main"),
        env: BTreeMap::from([("BASE".to_string(), "it's main".to_string())]),
        once_env: BTreeMap::from([("GH_TOKEN".to_string(), "sekrit".to_string())]),
        passthrough_env: vec!["IGNORED".into()],
        files: vec![
            ("/work/boot.sh".into(), "echo boot\n".into()),
            ("/work/source/README.md".into(), "# hi\n".into()),
        ],
        command: vec!["bash".into(), "/work/boot.sh".into()],
        build_dir: "target".into(),
        tmpfs: String::new(),
    }
}

/// Make slot `index` of `pool` on disk (not started: starting needs Linux).
fn make(p: &SshProvider, pool: &str, index: u32) -> String {
    let spec = spec(pool, index);
    let id = new_slot_id(&spec).unwrap();
    let archive = SshProvider::new_slot_archive(&spec).unwrap();
    p.require("create", &[&id], Some(&archive)).unwrap();
    id
}

fn ready(p: &SshProvider, id: &str) {
    p.seal(id, &["GH_TOKEN"], META_STATE, STATE_READY).unwrap();
}

fn slot_dir(home: &Path, id: &str) -> PathBuf {
    home.join("lanes").join(id)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_new_slot_holds_marks_environment_files_and_a_private_one_time_secret() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    assert_eq!(id, "ms/lanes-a/slot-1");
    assert_eq!(
        std::fs::read_to_string(slot_dir(home.path(), &id).join("marks/vivi_pool_mailspace"))
            .unwrap(),
        "ms\n"
    );
    let dir = slot_dir(home.path(), &id);
    let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).unwrap();
    assert_eq!(read("marks/vivi_pool"), "lanes-a\n");
    assert_eq!(read("marks/vivi_pool_slot"), "1\n");
    assert_eq!(read("marks/vivi_pool_state"), "seeding\n");
    assert!(!read("marks/vivi_pool_arch").trim().is_empty());
    assert_eq!(read("slot.env"), "BASE='it'\\''s main'\n");
    assert_eq!(read("boot.sh"), "echo boot\n");
    assert_eq!(read("source/README.md"), "# hi\n");
    assert!(read("once.env").contains("export GH_TOKEN='sekrit'"));
    assert_eq!(mode(&dir.join("once.env")), 0o600, "secret is private");
    assert_eq!(mode(&dir), 0o700, "the slot is private to the login user");
    assert!(
        !read("slot.env").contains("sekrit"),
        "no secret in the environment file"
    );
    assert!(home.path().join("lanes/.vivi-root").is_file());
}

#[test]
fn the_slot_is_listed_from_the_host_with_state_marks_and_no_local_record() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    assert!(p.list().unwrap().is_empty(), "no root yet is an empty list");
    let id = make(&p, "lanes-a", 1);
    let found = p.list().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, id);
    assert_eq!(found[0].name, "ms-lanes-a-slot-1");
    assert_eq!(found[0].region, "box");
    assert_eq!(found[0].state, "stopped");
    assert_eq!(slot_state(&found[0]), "unseeded");
    ready(&p, &id);
    let after = p.machine(&id).unwrap();
    assert_eq!(slot_state(&after), "free");
    assert_eq!(after.metadata[META_STATE], STATE_READY);
    assert!(!after.created_at.is_empty());
}

#[test]
fn a_slot_that_is_not_there_reads_as_gone_and_a_bad_id_never_reaches_the_host() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    assert_eq!(p.machine("ms/lanes-a/slot-9").unwrap().state, "destroyed");
    for bad in ["../x/slot-1", "a/slot-1/..", "slot-1", "a b/slot-1", "/etc"] {
        let said = p.machine(bad).unwrap_err().to_string();
        assert!(said.contains("not a slot id"), "{said}");
        assert!(p.start(bad).is_err() && p.stop(bad).is_err() && p.destroy(bad).is_err());
        assert!(p.claim(bad, "t", 1).is_err());
    }
}

#[test]
fn a_claim_marks_the_slot_with_task_and_time_and_unclaim_frees_it() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    ready(&p, &id);
    let hold = p.claim(&id, "task-abc", 1_700_000_000).unwrap().unwrap();
    assert!(!hold.nonce.is_empty());
    let marked = p.machine(&id).unwrap();
    assert_eq!(marked.metadata[META_TASK], "task-abc");
    assert_eq!(marked.metadata[META_CLAIMED], "1700000000");
    assert_eq!(slot_state(&marked), "finished-uncollected");
    assert_eq!(p.claim(&id, "task-other", 2).unwrap(), None, "it is taken");
    p.release(&id, &hold).unwrap();
    assert!(p.machine(&id).unwrap().metadata.contains_key(META_TASK));
    p.unclaim(&id).unwrap();
    assert_eq!(slot_state(&p.machine(&id).unwrap()), "free");
    p.unclaim(&id).unwrap();
    assert!(p.claim(&id, "task-again", 3).unwrap().is_some());
}

#[test]
fn a_slot_that_is_not_ready_cannot_be_claimed_and_keeps_no_claim() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    assert_eq!(p.claim(&id, "task-abc", 1).unwrap(), None);
    assert!(!slot_dir(home.path(), &id).join("claim").exists());
    let said = p.claim("ms/lanes-a/slot-7", "task-abc", 1).unwrap_err();
    assert!(said.to_string().contains("gone"), "{said}");
}

#[test]
fn many_claimants_at_once_yield_exactly_one_winner() {
    let home = tempfile::tempdir().unwrap();
    let base = provider(home.path());
    let id = make(&base, "lanes-a", 1);
    ready(&base, &id);
    // Each claimant is a provider of its own with its own process per call,
    // as two `vivi` processes on two machines are.
    let wins: Vec<bool> = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..16)
            .map(|n| {
                let (p, id) = (provider(home.path()), id.clone());
                scope.spawn(move || p.claim(&id, &format!("racer{n:02}"), 7).unwrap().is_some())
            })
            .collect();
        tasks.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert_eq!(wins.iter().filter(|w| **w).count(), 1, "{wins:?}");
    let winner = base.machine(&id).unwrap().metadata[META_TASK].clone();
    let n: usize = winner.trim_start_matches("racer").parse().unwrap();
    assert!(wins[n], "the recorded task is the winner's: {winner}");
    let leftovers: Vec<_> = std::fs::read_dir(slot_dir(home.path(), &id))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "losers clean up their files");
}

#[test]
fn a_claim_that_appears_between_look_and_take_is_a_taken_not_a_double_claim() {
    // The simulated race: the listing said free, then someone else claimed.
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    ready(&p, &id);
    let stale = p.list().unwrap();
    assert_eq!(slot_state(&stale[0]), "free");
    let first = p.claim(&id, "task-first", 1).unwrap();
    assert!(first.is_some());
    assert_eq!(p.claim(&id, "task-second", 2).unwrap(), None);
    assert_eq!(p.machine(&id).unwrap().metadata[META_TASK], "task-first");
}

#[test]
fn only_the_claim_holder_can_deliver_a_file_and_it_lands_atomically_and_private() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    ready(&p, &id);
    let hold = p.claim(&id, "task-abc", 1).unwrap().unwrap();
    let stranger = Hold {
        nonce: "not-it".into(),
    };
    assert!(p.put_file(&id, &stranger, "/work/job.tgz", b"x").is_err());
    assert!(!slot_dir(home.path(), &id).join("job.tgz").exists());
    let payload: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
    p.put_file(&id, &hold, "/work/job.tgz", &payload).unwrap();
    let path = slot_dir(home.path(), &id).join("job.tgz");
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    assert_eq!(mode(&path), 0o600);
    assert!(!slot_dir(home.path(), &id).join("job.tgz.partial").exists());
    for bad in ["/etc/passwd", "/work/../x", "/work/a//b", "/work/"] {
        assert!(p.put_file(&id, &hold, bad, b"x").is_err(), "{bad}");
    }
}

#[test]
fn a_stopped_slot_is_read_in_place_and_paths_come_back_as_the_pool_spells_them() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    let job = slot_dir(home.path(), &id).join("job");
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(job.join("result"), "job=abc\npush=pushed\n").unwrap();
    std::fs::write(job.join("agent.out"), "receipt\n").unwrap();
    let paths = [
        "/work/job/result",
        "/work/job/agent.out",
        "/work/job/run.log",
    ];
    let files = p.read_stopped(&id, &paths).unwrap().unwrap();
    assert_eq!(files.len(), 2, "a file that is not there is left out");
    assert_eq!(files["/work/job/result"], b"job=abc\npush=pushed\n");
    assert_eq!(files["/work/job/agent.out"], b"receipt\n");
    assert!(p.read_stopped(&id, &["/etc/passwd"]).is_err());
    let gone = p
        .read_stopped("ms/lanes-a/slot-5", &paths)
        .unwrap()
        .unwrap();
    assert!(gone.is_empty());
}

#[test]
fn exec_runs_in_the_slot_with_work_set_and_returns_the_scripts_own_exit_code() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    let out = p
        .exec(&id, "echo \"$WORK\"; pwd; echo oops >&2; exit 3", 20)
        .unwrap();
    let dir = std::fs::canonicalize(slot_dir(home.path(), &id)).unwrap();
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines[0], dir.to_str().unwrap());
    assert_eq!(lines[1], dir.to_str().unwrap());
    assert_eq!(out.stderr.trim(), "oops");
    assert_eq!(out.exit_code, 3);
}

#[test]
fn the_collect_script_reads_a_result_through_exec_under_the_slot_root() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    let job = slot_dir(home.path(), &id).join("job");
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(job.join("result"), "job=abc\npush=none\n").unwrap();
    std::fs::write(job.join("agent.out"), "the receipt\n").unwrap();
    let script = crate::pool::collect::COLLECT_SCRIPT;
    let out = p.exec(&id, script, 20).unwrap();
    let got = crate::pool::collect::parse_collected(&out.stdout).unwrap();
    assert_eq!(got.result.get("job"), "abc");
    assert!(got.receipt.contains("the receipt"));
}

#[test]
fn hostile_values_stay_data_and_never_reach_a_shell() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    ready(&p, &id);
    let marker = home.path().join("pwned");
    let m = marker.display();
    let task = format!("a'; touch {m} ; echo '\"$(touch {m})\"`touch {m}`");
    p.claim(&id, &task, 1).unwrap().unwrap();
    assert_eq!(p.machine(&id).unwrap().metadata[META_TASK], task);
    let out = p
        .exec(&id, &format!("printf '%s' '$(touch {m})'"), 20)
        .unwrap();
    assert!(out.stdout.starts_with("$(touch"), "{}", out.stdout);
    assert!(!marker.exists(), "nothing was executed");
}

#[test]
fn a_value_with_a_line_break_is_refused_before_it_is_sent() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    assert!(p.claim(&id, "a\nclaimed=1", 1).is_err());
    assert!(p.seal(&id, &[], META_STATE, "ready\nx").is_err());
    assert!(p.seal(&id, &[], "bad name", "v").is_err());
}

#[test]
fn creating_over_an_existing_slot_is_refused_and_leaves_it_alone() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    ready(&p, &id);
    let archive = SshProvider::new_slot_archive(&spec("lanes-a", 1)).unwrap();
    let err = p.require("create", &[&id], Some(&archive)).unwrap_err();
    assert!(err.to_string().contains("exists"), "{err}");
    assert_eq!(slot_state(&p.machine(&id).unwrap()), "free");
}

#[test]
fn harness_names_come_from_the_login_environment_and_only_the_names_are_asked() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let names = vec![
        "VIVI_TEST_SURELY_UNSET_A".to_string(),
        "HOME".to_string(),
        "VIVI_TEST_SURELY_UNSET_B".to_string(),
    ];
    let gaps = p.harness_env_gaps(&names).unwrap();
    assert_eq!(
        gaps,
        ["VIVI_TEST_SURELY_UNSET_A", "VIVI_TEST_SURELY_UNSET_B"]
    );
    assert_eq!(p.env_gaps_at_dispatch(&names).unwrap(), gaps);
    assert!(p.harness_env_gaps(&[]).unwrap().is_empty());
    let hint = p.env_gap_hint(&gaps);
    assert!(hint.contains("login environment") && hint.contains("~/.profile"));
    assert!(!hint.contains('='), "never a value");
}

#[test]
fn the_provider_says_what_a_directory_slot_is() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    assert!(p.build_persists() && p.runs_unprivileged() && !p.supplies_build_mount());
    assert_eq!(p.ensure_home().unwrap(), None);
    assert_eq!(
        p.destroy_command("ms/lanes-a/slot-1"),
        "ssh box rm -rf ~/lanes/ms/lanes-a/slot-1"
    );
    assert!(p.live_test_notice().contains("~/lanes on box"));
}

struct Down;

impl Runner for Down {
    fn run(&self, _: &[String], _: Option<&[u8]>, _: Duration) -> Result<RunOutput, VivariumError> {
        Ok(RunOutput {
            code: 255,
            stdout: Vec::new(),
            stderr: "ssh: connect to host box port 22: Connection refused\n".into(),
        })
    }
}

#[test]
fn a_failed_connection_is_named_with_the_host_and_is_not_a_script_failure() {
    let p = SshProvider::with_runner(&config("~/lanes"), Box::new(Down));
    let said = p.list().unwrap_err().to_string();
    assert!(said.contains("cannot reach box over ssh"), "{said}");
    assert!(said.contains("Connection refused"), "{said}");
    assert!(p.exec("ms/lanes-a/slot-1", "true", 5).is_err());
}

// --- destroy: the one recursive delete on someone's server ---

/// Run `destroy` of `id` with the raw script, for a root spelled `root`.
fn raw_destroy(home: &Path, root: &str, id: &str) -> (i32, String) {
    let line = command_line(root, "destroy", &[id], false);
    let shell = Shell {
        home: home.to_path_buf(),
    };
    let out = shell.run(&[line], None, Duration::from_secs(30)).unwrap();
    (out.code, format!("{}{}", out.stdout_text(), out.stderr))
}

fn refused(result: &(i32, String)) -> bool {
    result.0 == 92 && result.1.contains("refusing to delete")
}

#[test]
fn destroy_removes_the_slot_its_pool_directory_and_a_root_it_made() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let a = make(&p, "lanes-a", 1);
    let b = make(&p, "lanes-a", 2);
    p.destroy(&a).unwrap();
    assert!(!slot_dir(home.path(), &a).exists());
    assert!(slot_dir(home.path(), &b).exists());
    assert!(home.path().join("lanes").exists());
    p.destroy(&b).unwrap();
    p.destroy(&b).unwrap();
    assert!(
        !home.path().join("lanes").exists(),
        "the root vivi made goes"
    );
}

#[test]
fn destroy_leaves_a_root_it_did_not_make_and_other_pools_and_files_alone() {
    let home = tempfile::tempdir().unwrap();
    let lanes = home.path().join("lanes");
    std::fs::create_dir_all(lanes.join("keep-me")).unwrap();
    std::fs::write(lanes.join("keep-me/file.txt"), "mine").unwrap();
    let p = provider(home.path());
    let a = make(&p, "lanes-a", 1);
    let other = make(&p, "lanes-b", 1);
    p.destroy(&a).unwrap();
    assert!(!lanes.join("ms/lanes-a").exists());
    assert!(lanes.join("ms/lanes-b/slot-1").exists(), "{other}");
    assert!(lanes.join("keep-me/file.txt").exists());
    p.destroy(&other).unwrap();
    assert!(lanes.join("keep-me/file.txt").exists());
    assert!(lanes.exists(), "a root that was already there stays");
}

#[test]
fn destroy_refuses_a_directory_without_vivis_mark() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("lanes/ms/lanes-a/slot-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("precious.txt"), "do not delete").unwrap();
    let result = raw_destroy(home.path(), "~/lanes", "ms/lanes-a/slot-1");
    assert!(refused(&result), "{result:?}");
    assert!(result.1.contains("no Vivi mark"));
    assert!(dir.join("precious.txt").exists());
    let said = provider(home.path())
        .destroy("ms/lanes-a/slot-1")
        .unwrap_err()
        .to_string();
    assert!(said.contains("refusing to delete"), "{said}");
    assert!(dir.join("precious.txt").exists());
}

#[test]
fn destroy_refuses_a_mark_that_names_another_pool() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    let other = home.path().join("lanes/ms/lanes-b");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::rename(slot_dir(home.path(), &id), other.join("slot-1")).unwrap();
    let result = raw_destroy(home.path(), "~/lanes", "ms/lanes-b/slot-1");
    assert!(refused(&result), "{result:?}");
    assert!(other.join("slot-1/marks/vivi_pool").exists());
}

#[test]
fn destroy_refuses_ids_that_climb_out_of_the_root() {
    let home = tempfile::tempdir().unwrap();
    let victim = home.path().join("victim/slot-1/marks");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("vivi_pool"), "victim\n").unwrap();
    std::fs::create_dir_all(home.path().join("lanes")).unwrap();
    for id in [
        "../victim/slot-1",
        "victim/../victim/slot-1",
        "/victim/slot-1",
        "ms/lanes-a/slot-1/../../..",
        "ms/lanes-a/slot-1/x",
        "",
        "ms/lanes-a",
        "lanes-a/slot-1",
    ] {
        let result = raw_destroy(home.path(), "~/lanes", id);
        assert!(refused(&result), "{id:?}: {result:?}");
    }
    assert!(victim.join("vivi_pool").exists());
}

#[test]
fn destroy_refuses_an_empty_or_unsafe_root_however_the_slot_looks() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    make(&p, "lanes-a", 1);
    let h = home.path().to_str().unwrap();
    for root in ["", "/", "lanes", h] {
        let result = raw_destroy(home.path(), root, "ms/lanes-a/slot-1");
        assert!(refused(&result), "{root:?}: {result:?}");
        assert!(
            slot_dir(home.path(), "ms/lanes-a/slot-1").exists(),
            "{root:?}"
        );
    }
}

#[test]
fn destroy_refuses_a_slot_that_is_a_link_or_lies_behind_one() {
    let home = tempfile::tempdir().unwrap();
    let outside = home.path().join("outside");
    std::fs::create_dir_all(outside.join("marks")).unwrap();
    std::fs::write(outside.join("marks/vivi_pool"), "lanes-a\n").unwrap();
    std::fs::write(outside.join("precious"), "x").unwrap();
    let pool = home.path().join("lanes/ms/lanes-a");
    std::fs::create_dir_all(&pool).unwrap();
    std::os::unix::fs::symlink(&outside, pool.join("slot-1")).unwrap();
    let result = raw_destroy(home.path(), "~/lanes", "ms/lanes-a/slot-1");
    assert!(refused(&result), "a linked slot: {result:?}");
    assert!(outside.join("precious").exists());
    std::fs::remove_file(pool.join("slot-1")).unwrap();
    std::fs::remove_dir(&pool).unwrap();
    let behind = home.path().join("outside-pool");
    std::fs::create_dir_all(behind.join("slot-1/marks")).unwrap();
    std::fs::write(behind.join("slot-1/marks/vivi_pool"), "lanes-a\n").unwrap();
    std::fs::write(behind.join("slot-1/precious"), "x").unwrap();
    std::os::unix::fs::symlink(&behind, &pool).unwrap();
    let result = raw_destroy(home.path(), "~/lanes", "ms/lanes-a/slot-1");
    assert!(refused(&result), "a linked pool directory: {result:?}");
    assert!(behind.join("slot-1/precious").exists());
}

#[test]
fn the_destroy_guard_holds_through_the_provider_for_every_unsafe_id() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    std::fs::create_dir_all(home.path().join("lanes")).unwrap();
    let keep = home.path().join("keep.txt");
    std::fs::write(&keep, "x").unwrap();
    for id in ["..", "../..", "lanes/..", "~", "/", "*", "a/b/slot-1;ls"] {
        assert!(p.destroy(id).is_err(), "{id}");
    }
    assert!(keep.exists());
}

#[test]
fn providers_share_nothing_but_the_host() {
    let home = tempfile::tempdir().unwrap();
    let first = Arc::new(provider(home.path()));
    let id = make(&first, "lanes-a", 1);
    let second = provider(home.path());
    assert_eq!(second.list().unwrap().len(), 1);
    assert_eq!(second.machine(&id).unwrap().id, id);
}

#[test]
fn a_new_slot_records_when_it_was_made() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    let before = chrono::Utc::now();
    let listed = p.machine(&id).unwrap();
    let made = chrono::DateTime::parse_from_rfc3339(&listed.created_at)
        .unwrap_or_else(|e| panic!("created_at {:?}: {e}", listed.created_at));
    assert!(
        made.timestamp() > 1_700_000_000,
        "not the epoch: {}",
        listed.created_at
    );
    let age = before.signed_duration_since(made).num_seconds();
    assert!(
        (0..60).contains(&age),
        "made {age}s ago: {}",
        listed.created_at
    );
    assert_eq!(p.list().unwrap()[0].created_at, listed.created_at);
}

/// A second mailspace's pool of the same name on the same host and root.
fn make_in(p: &SshProvider, space: &str, pool: &str, index: u32) -> String {
    let mut spec = spec(pool, index);
    spec.name = format!("{space}-{pool}-slot-{index}");
    spec.metadata = slot_metadata(&PoolId::new(space, pool), index, "main");
    let id = new_slot_id(&spec).unwrap();
    let archive = SshProvider::new_slot_archive(&spec).unwrap();
    p.require("create", &[&id], Some(&archive)).unwrap();
    id
}

#[test]
fn two_mailspaces_with_one_pool_name_keep_separate_directories_and_marks() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let mine = make_in(&p, "ms", "local", 1);
    let theirs = make_in(&p, "other", "local", 1);
    assert_eq!(
        (mine.as_str(), theirs.as_str()),
        ("ms/local/slot-1", "other/local/slot-1")
    );
    let listed = p.list().unwrap();
    assert_eq!(listed.len(), 2);
    let pool = |space: &str| PoolId::new(space, "local");
    let ours = crate::pool::slot::pool_slots(&pool("ms"), &listed);
    let others = crate::pool::slot::pool_slots(&pool("other"), &listed);
    assert_eq!((ours.len(), others.len()), (1, 1));
    assert_eq!(ours[0].id, mine);
    assert_eq!(others[0].id, theirs);
    p.destroy(&mine).unwrap();
    assert!(
        slot_dir(home.path(), &theirs).exists(),
        "the other mailspace's slot is untouched"
    );
    assert!(
        !home.path().join("lanes/ms").exists(),
        "the emptied mailspace directory goes"
    );
}

#[test]
fn destroy_refuses_a_slot_whose_mailspace_mark_is_not_the_path() {
    let home = tempfile::tempdir().unwrap();
    let p = provider(home.path());
    let id = make(&p, "lanes-a", 1);
    let dir = slot_dir(home.path(), &id);
    std::fs::write(dir.join("marks/vivi_pool_mailspace"), "other\n").unwrap();
    let result = raw_destroy(home.path(), "~/lanes", &id);
    assert!(refused(&result), "{result:?}");
    assert!(result.1.contains("mailspace"), "{result:?}");
    assert!(dir.exists());
    std::fs::remove_file(dir.join("marks/vivi_pool_mailspace")).unwrap();
    let result = raw_destroy(home.path(), "~/lanes", &id);
    assert!(
        refused(&result),
        "an unmarked mailspace is refused too: {result:?}"
    );
    assert!(dir.exists());
}
