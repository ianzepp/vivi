//! Against a real Linux host over SSH: what the unit tests cannot show, the
//! processes. Ignored by default because it needs a host; run it with
//! `VIVI_SSH_TEST_HOST=pharos cargo test --test pool_ssh_live -- --ignored --nocapture`
//! (optionally `VIVI_SSH_TEST_ROOT=~/vivi-pool-test`).
//!
//! It makes slots under the pool name `vivi-livetest`, starts only small
//! `sleep` scripts of its own (never a real boot), and removes everything
//! again, also after a failure.

use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use vivi::pool::PoolConfig;
use vivi::pool::provider::{CreateSpec, PoolProvider};
use vivi::pool::slot::{self, META_STATE, PoolId, STATE_READY};
use vivi::pool::ssh::SshProvider;

/// Each test has a pool of its own, so tests may run side by side.
const POOL_STOP: &str = "vivi-livetest-stop";
const POOL_EXIT: &str = "vivi-livetest-exit";
const POOL_CLAIM: &str = "vivi-livetest-claim";
const POOL_DESTROY: &str = "vivi-livetest-destroy";
const RACERS: usize = 8;

fn host() -> String {
    std::env::var("VIVI_SSH_TEST_HOST").expect("set VIVI_SSH_TEST_HOST")
}

fn root() -> String {
    std::env::var("VIVI_SSH_TEST_ROOT").unwrap_or_else(|_| "~/vivi-pool-test".to_string())
}

fn config() -> PoolConfig {
    toml::from_str(&format!(
        "repo = \"o/r\"\nslots = 2\nbuild_dir = \"target\"\nprovider = \"ssh\"\n[ssh]\nhost = \"{}\"\nroot = \"{}\"\n[github]\napp_id = 1\nprivate_key = \"k.pem\"\n",
        host(),
        root()
    ))
    .unwrap()
}

/// A slot whose boot script is `boot`, not the real one.
fn spec(pool: &str, index: u32, boot: &str) -> CreateSpec {
    CreateSpec {
        name: PoolId::new("livetest", pool).slot_name(index),
        image: String::new(),
        metadata: slot::slot_metadata(&PoolId::new("livetest", pool), index, "main"),
        env: Default::default(),
        once_env: Default::default(),
        passthrough_env: Vec::new(),
        files: vec![("/work/boot.sh".into(), boot.into())],
        command: vec!["bash".into(), "/work/boot.sh".into()],
        build_dir: Some("target".into()),
        tmpfs: String::new(),
    }
}

fn id_of(pool: &str, index: u32) -> String {
    format!("livetest/{pool}/slot-{index}")
}

/// Removes what the test made, whatever happens.
struct Cleanup(SshProvider, Vec<String>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for id in &self.1 {
            if let Err(e) = self.0.destroy(id) {
                eprintln!("cleanup: could not remove {id}: {e}");
            }
        }
    }
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// How many processes on the host match `pattern` (a regex that does not
/// match its own command line).
fn count(p: &SshProvider, id: &str, pattern: &str) -> usize {
    let script = format!("ps -eo args= | grep -c '^{pattern}' || true");
    let out = p.exec(id, &script, 20).unwrap();
    out.stdout.trim().parse().unwrap()
}

#[test]
#[ignore = "needs an SSH host"]
fn stopping_a_slot_ends_everything_it_started_and_only_that() {
    let mut made = Cleanup(SshProvider::new(&config()), Vec::new());
    let boot = "sleep 311 &\nsetsid sleep 312 < /dev/null > /dev/null 2>&1 &\nsleep 313\n";
    let id = made.0.create(&spec(POOL_STOP, 1, boot)).unwrap();
    made.1.push(id.clone());
    let p = &made.0;
    assert_eq!(id, id_of(POOL_STOP, 1));
    wait_for("the slot to read started", || {
        p.machine(&id).unwrap().state == "started"
    });
    wait_for("the sleepers", || count(p, &id, "sleep 31[123]$") == 3);
    println!("started: three sleepers, one of them in a session of its own");

    // A stranger's process whose pid is in boot.pid is not the slot.
    p.exec(
        &id,
        "(sleep 314 < /dev/null > /dev/null 2>&1 & echo $! > \"$WORK/other.pid\")",
        20,
    )
    .unwrap();
    assert_eq!(count(p, &id, "sleep 314$"), 1);

    let started = Instant::now();
    p.stop(&id).unwrap();
    println!("stop took {:?}", started.elapsed());
    assert_eq!(p.machine(&id).unwrap().state, "stopped");
    assert_eq!(
        count(p, &id, "sleep 31[123]$"),
        0,
        "nothing outlives a stop"
    );
    assert_eq!(count(p, &id, "sleep 314$"), 1, "a stranger is left alone");

    // A stale pid file that now names someone else's process is not "running".
    p.exec(&id, "cp \"$WORK/other.pid\" \"$WORK/boot.pid\"", 20)
        .unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "stopped");
    p.stop(&id).unwrap();
    assert_eq!(
        count(p, &id, "sleep 314$"),
        1,
        "stop does not kill a reused pid"
    );
    p.exec(&id, "kill $(cat \"$WORK/other.pid\")", 20).unwrap();
    assert_eq!(count(p, &id, "sleep 314$"), 0);
}

#[test]
#[ignore = "needs an SSH host"]
fn a_boot_that_ends_reads_stopped_with_its_exit_code_and_start_is_idempotent() {
    let mut made = Cleanup(SshProvider::new(&config()), Vec::new());
    let id = made
        .0
        .create(&spec(POOL_EXIT, 1, "sleep 2\nexit 7\n"))
        .unwrap();
    made.1.push(id.clone());
    let p = &made.0;
    p.start(&id).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "started");
    wait_for("the boot to end", || {
        p.machine(&id).unwrap().state == "stopped"
    });
    assert_eq!(p.machine(&id).unwrap().exit_code, Some(7));
    p.stop(&id).unwrap();
    p.start(&id).unwrap();
    assert_eq!(
        p.machine(&id).unwrap().exit_code,
        None,
        "a new run has no exit yet"
    );
}

#[test]
#[ignore = "needs an SSH host"]
fn the_claim_is_atomic_across_separate_ssh_connections_and_calls_are_timed() {
    let mut made = Cleanup(SshProvider::new(&config()), Vec::new());
    let id = made.0.create(&spec(POOL_CLAIM, 1, "true\n")).unwrap();
    made.1.push(id.clone());
    let provider = Arc::new(SshProvider::new(&config()));
    wait_for("the boot to end", || {
        provider.machine(&id).unwrap().state == "stopped"
    });
    provider.seal(&id, &[], META_STATE, STATE_READY).unwrap();

    let started = Instant::now();
    for _ in 0..10 {
        provider.machine(&id).unwrap();
    }
    println!(
        "10 calls over one shared connection: {:?} each",
        started.elapsed() / 10
    );

    // Each racer is a provider of its own: its own `ssh`, its own connection.
    // The connections are made one after another first (a host limits how
    // many new SSH connections may start at once), then all claim together.
    let gate = Barrier::new(RACERS);
    let wins: Vec<bool> = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..RACERS)
            .map(|n| {
                let (id, gate) = (id.clone(), &gate);
                scope.spawn(move || {
                    let p = SshProvider::new(&config());
                    std::thread::sleep(Duration::from_millis(250 * n as u64));
                    p.machine(&id).unwrap();
                    gate.wait();
                    p.claim(&id, &format!("racer{n:03}"), 1).unwrap().is_some()
                })
            })
            .collect();
        tasks.into_iter().map(|t| t.join().unwrap()).collect()
    });
    println!("claim race wins: {wins:?}");
    assert_eq!(wins.iter().filter(|w| **w).count(), 1, "{wins:?}");
    let held = provider.machine(&id).unwrap();
    assert!(held.metadata["vivi_pool_task"].starts_with("racer"));
    assert_eq!(slot::slot_state(&held), "finished-uncollected");
    provider.unclaim(&id).unwrap();
    assert_eq!(slot::slot_state(&provider.machine(&id).unwrap()), "free");
}

#[test]
#[ignore = "needs an SSH host"]
fn destroy_leaves_nothing_behind_and_a_foreign_directory_is_refused() {
    let p = SshProvider::new(&config());
    let id = p.create(&spec(POOL_DESTROY, 1, "sleep 300\n")).unwrap();
    p.destroy(&id).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "destroyed");
    // A directory of the user's own under the root is not a slot.
    let dir = format!("{}/livetest/{POOL_DESTROY}/slot-9", root());
    let make = format!("mkdir -p {dir} && echo mine > {dir}/keep");
    let out = Command::new("ssh")
        .args(["-o", "BatchMode=yes", &host(), &make])
        .output()
        .unwrap();
    assert!(out.status.success());
    let refused = p.destroy(&id_of(POOL_DESTROY, 9)).unwrap_err().to_string();
    assert!(refused.contains("refusing to delete"), "{refused}");
    let check = format!(
        "cat {dir}/keep; rm -r {r}/livetest/{POOL_DESTROY}; true",
        r = root()
    );
    let out = Command::new("ssh")
        .args(["-o", "BatchMode=yes", &host(), &check])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "mine");
}
