//! The provider against the real worker driver, run on this machine instead
//! of over SSH, with `dir` boundaries in a temporary root (no disk images or
//! RAM disks are made here; those are covered live on a Mac worker).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::*;
use crate::pool::docker_cli::{RunOutput, finish};
use crate::pool::progress::Progress;
use crate::pool::slot::{META_MAILSPACE, PoolId};

/// Runs a remote command line locally, with `home` as the login home.
struct Shell {
    home: PathBuf,
    /// A directory put first on the PATH of the session, if any.
    path_first: Option<PathBuf>,
}

impl Runner for Shell {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let mut command = Command::new("sh");
        if let Some(first) = &self.path_first {
            let path = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{path}", first.display()));
        }
        let child = command
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

/// A worker home with its root and `worker.conf`.
fn home(conf: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("vivi-worker");
    std::fs::create_dir_all(&root).unwrap();
    let dirs = home.path().join("dirs");
    std::fs::write(
        root.join("worker.conf"),
        format!("dir_root={}\n{conf}", dirs.display()),
    )
    .unwrap();
    home
}

fn config(slots: u32, kind: &str, size: u32) -> PoolConfig {
    toml::from_str(&format!(
        "slots = {slots}\nprovider = \"worker\"\n[worker]\nhost = \"studio\"\nkind = \"{kind}\"\nsize_gib = {size}\n"
    ))
    .unwrap()
}

fn provider(home: &Path, slots: u32, kind: &str, size: u32) -> WorkerProvider {
    provider_with_path(home, slots, kind, size, None)
}

fn provider_with_path(
    home: &Path,
    slots: u32,
    kind: &str,
    size: u32,
    path_first: Option<PathBuf>,
) -> WorkerProvider {
    WorkerProvider::with_runner(
        &config(slots, kind, size),
        "ms",
        "lanes",
        Box::new(Shell {
            home: home.to_path_buf(),
            path_first,
        }),
    )
    .with_seed_pacing(Duration::from_secs(20), Duration::from_millis(50))
}

fn spec(boot: &str) -> CreateSpec {
    let metadata = [
        (META_MAILSPACE.to_string(), "ms".to_string()),
        (META_POOL.to_string(), "lanes".to_string()),
    ]
    .into_iter()
    .collect();
    CreateSpec {
        name: String::new(),
        image: String::new(),
        metadata,
        env: BTreeMap::new(),
        once_env: BTreeMap::new(),
        passthrough_env: Vec::new(),
        files: vec![("/work/boot.sh".to_string(), boot.to_string())],
        command: Vec::new(),
        build_dir: None,
        tmpfs: String::new(),
    }
}

fn open(p: &WorkerProvider, boot: &str) -> Result<OpenedSlot, VivariumError> {
    let id = PoolId {
        mailspace: "ms".to_string(),
        pool: "lanes".to_string(),
    };
    let progress = Progress::silent();
    p.open_for_dispatch(&OpenRequest {
        pool: &id,
        task: "t1",
        now: 1,
        budget_secs: 60,
        spec: spec(boot),
        progress: &progress,
        ready_timeout_secs: None,
    })
}

fn empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |mut d| d.next().is_none())
}

#[test]
fn a_dispatch_builds_seeds_and_claims_a_slot_and_destroy_removes_it() {
    let home = home("");
    let p = provider(home.path(), 2, "dir", 0);
    let slot = open(&p, "echo seeded > seed.txt\n").unwrap();
    assert_eq!(slot.id, "ms/lanes/slot-1");
    assert_eq!(slot.index, 1);
    assert!(!slot.hold.nonce.is_empty());
    let seen = p.machine(&slot.id).unwrap();
    assert_eq!(seen.state, "stopped");
    assert_eq!(seen.exit_code, Some(0));
    assert_eq!(
        seen.metadata.get(META_STATE).map(String::as_str),
        Some(STATE_READY)
    );
    let files = p
        .read_stopped(&slot.id, &["/work/seed.txt"])
        .unwrap()
        .unwrap();
    assert_eq!(files["/work/seed.txt"], b"seeded\n");
    assert!(
        home.path()
            .join("dirs/ms-lanes-slot-1/bin/timeout")
            .exists(),
        "a worker slot carries the GNU stand-ins"
    );
    p.destroy(&slot.id).unwrap();
    assert!(p.list().unwrap().is_empty());
    assert!(empty(&home.path().join("dirs")), "the dir boundary is gone");
    assert!(
        empty(&home.path().join("vivi-worker/.ledger")),
        "the ledger is empty"
    );
}

#[test]
fn a_full_pool_is_no_free_slot_and_the_next_index_is_used() {
    let home = home("");
    let p = provider(home.path(), 2, "dir", 0);
    let first = open(&p, "true\n").unwrap();
    let second = open(&p, "true\n").unwrap();
    assert_eq!(second.index, 2);
    let said = open(&p, "true\n").unwrap_err().to_string();
    assert!(said.contains("no free slot in pool lanes"), "{said}");
    p.destroy(&first.id).unwrap();
    assert_eq!(
        open(&p, "true\n").unwrap().index,
        1,
        "a freed index is reused"
    );
}

#[test]
fn a_request_over_the_worker_budget_is_no_free_slot_and_leaves_nothing() {
    let home = home("ram_budget_gib=0\n");
    let p = provider(home.path(), 2, "ram", 4);
    let said = open(&p, "true\n").unwrap_err().to_string();
    assert!(
        said.contains("no free slot") && said.contains("exceeds the budget"),
        "{said}"
    );
    assert!(p.list().unwrap().is_empty());
    assert!(empty(&home.path().join("vivi-worker/.ledger")));
}

#[test]
fn a_failed_seed_is_reported_and_its_slot_removed() {
    let home = home("");
    let p = provider(home.path(), 1, "dir", 0);
    let said = open(&p, "echo nope >&2\nexit 5\n").unwrap_err().to_string();
    assert!(said.contains("failed (exit 5)"), "{said}");
    assert!(p.list().unwrap().is_empty());
    assert!(empty(&home.path().join("dirs")));
}

#[test]
fn a_worker_pool_makes_slots_only_per_dispatch() {
    let home = home("");
    let p = provider(home.path(), 1, "dir", 0);
    assert_eq!(p.slot_lifetime(), SlotLifetime::PerDispatch);
    assert!(p.create(&spec("true\n")).is_err());
}

#[test]
fn one_provider_serves_its_pool_and_the_doctor_test_pool() {
    let home = home("");
    let p = provider(home.path(), 1, "dir", 0);
    let mine = open(&p, "true\n").unwrap();
    let progress = Progress::silent();
    let doctor = PoolId {
        mailspace: "ms".to_string(),
        pool: "lanes-doctor".to_string(),
    };
    let mut doctor_spec = spec("true\n");
    doctor_spec
        .metadata
        .insert(META_POOL.to_string(), "lanes-doctor".to_string());
    let test = p
        .open_for_dispatch(&OpenRequest {
            pool: &doctor,
            task: "t2",
            now: 1,
            budget_secs: 60,
            spec: doctor_spec,
            progress: &progress,
            ready_timeout_secs: None,
        })
        .unwrap();
    assert_eq!(
        test.id, "ms/lanes-doctor/slot-1",
        "each pool counts its own slots"
    );
    assert_eq!(
        p.list().unwrap().len(),
        2,
        "the listing covers the mailspace"
    );
    p.destroy(&test.id).unwrap();
    p.destroy(&mine.id).unwrap();
    assert!(p.list().unwrap().is_empty());
}

#[test]
fn a_boot_keeps_the_session_path_order_through_a_login_shell_that_reorders_it() {
    let home = home("");
    // The login shell reads this after /etc/profile and puts system
    // directories first, as macOS path_helper does.
    std::fs::write(
        home.path().join(".bash_profile"),
        "PATH=/usr/bin:/bin:/usr/sbin:/sbin:$PATH; export PATH\n",
    )
    .unwrap();
    let first = home.path().join("session-first");
    let p = provider_with_path(home.path(), 1, "dir", 0, Some(first.clone()));
    let slot = open(&p, "echo \"$PATH\" > path.txt\n").unwrap();
    let files = p
        .read_stopped(&slot.id, &["/work/path.txt"])
        .unwrap()
        .unwrap();
    let path = String::from_utf8(files["/work/path.txt"].clone()).unwrap();
    let entries: Vec<&str> = path.trim_end().split(':').collect();
    assert_eq!(
        entries[0],
        first.to_str().unwrap(),
        "the session's first entry stays first: {path}"
    );
    let mut seen = std::collections::BTreeSet::new();
    assert!(
        entries.iter().all(|e| seen.insert(*e)),
        "no repeats: {path}"
    );
}

/// A runner that can lose a call after it ran (a dropped connection) or
/// refuse one before it runs.
struct Flaky {
    inner: Shell,
    /// This verb runs on the worker, then reports a failure.
    lose_after: Option<&'static str>,
    /// This verb never runs.
    refuse: Option<&'static str>,
}

impl Runner for Flaky {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let is = |verb: Option<&str>| {
            // The script is shell-quoted whole, so the verb's own quotes are
            // escaped; the verb is the word on the `VERB=` line.
            verb.is_some_and(|v| {
                let line = args[0].split("VERB=").nth(1).and_then(|r| r.lines().next());
                line.is_some_and(|l| l.replace(['\'', '\\'], "") == v)
            })
        };
        if is(self.refuse) {
            return Err(VivariumError::Message("ssh: connection refused".into()));
        }
        let out = self.inner.run(args, stdin, timeout)?;
        if is(self.lose_after) {
            return Err(VivariumError::Message("ssh: connection dropped".into()));
        }
        Ok(out)
    }
}

fn flaky_provider(
    home: &Path,
    slots: u32,
    lose_after: Option<&'static str>,
    refuse: Option<&'static str>,
) -> WorkerProvider {
    WorkerProvider::with_runner(
        &config(slots, "dir", 0),
        "ms",
        "lanes",
        Box::new(Flaky {
            inner: Shell {
                home: home.to_path_buf(),
                path_first: None,
            },
            lose_after,
            refuse,
        }),
    )
    .with_seed_pacing(Duration::from_secs(20), Duration::from_millis(50))
}

#[test]
fn two_dispatches_at_once_never_share_a_slot_index() {
    for _ in 0..3 {
        let home = home("");
        let p = provider(home.path(), 2, "dir", 0);
        let start = std::sync::Barrier::new(2);
        let opened: Vec<OpenedSlot> = std::thread::scope(|s| {
            let jobs: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        open(&p, "true\n").unwrap()
                    })
                })
                .collect();
            jobs.into_iter().map(|j| j.join().unwrap()).collect()
        });
        let mut indices: Vec<u32> = opened.iter().map(|o| o.index).collect();
        indices.sort_unstable();
        assert_eq!(indices, [1, 2], "each dispatch holds its own slot");
        assert_eq!(p.list().unwrap().len(), 2, "both slots stand");
        for slot in &opened {
            p.destroy(&slot.id).unwrap();
        }
        assert!(empty(&home.path().join("vivi-worker/.ledger")));
    }
}

#[test]
fn an_index_the_driver_holds_is_skipped_and_never_torn_down() {
    let home = home("");
    // A slot mid-build (or left by a failed build) has a ledger entry but no
    // marks, so the listing does not show it; the driver still holds the index.
    let ledger = home.path().join("vivi-worker/.ledger");
    std::fs::create_dir_all(&ledger).unwrap();
    let held = ledger.join("ms-lanes-slot-1");
    std::fs::write(&held, "kind=dir\nsize_gib=0\n").unwrap();
    let p = provider(home.path(), 2, "dir", 0);
    let slot = open(&p, "true\n").unwrap();
    assert_eq!(slot.index, 2, "the held index is skipped");
    assert!(held.exists(), "the other dispatch's slot is left alone");
    p.destroy(&slot.id).unwrap();

    let full = provider(home.path(), 1, "dir", 0);
    let said = open(&full, "true\n").unwrap_err().to_string();
    assert!(said.contains("no free slot in pool lanes"), "{said}");
    assert!(held.exists(), "a refused dispatch still leaves it alone");
}

#[test]
fn a_create_that_fails_after_it_ran_tears_its_slot_down() {
    let home = home("");
    let p = flaky_provider(home.path(), 2, Some("create"), None);
    let said = open(&p, "true\n").unwrap_err().to_string();
    assert!(said.contains("connection dropped"), "{said}");
    assert!(!said.contains("teardown"), "{said}");
    assert!(p.list().unwrap().is_empty(), "the slot is gone");
    assert!(empty(&home.path().join("dirs")), "its boundary is gone");
    assert!(empty(&home.path().join("vivi-worker/.ledger")));
}

#[test]
fn a_teardown_that_fails_is_part_of_the_error() {
    let home = home("");
    let p = flaky_provider(home.path(), 2, Some("create"), Some("destroy"));
    let said = open(&p, "true\n").unwrap_err().to_string();
    assert!(said.contains("connection dropped"), "{said}");
    assert!(
        said.contains("teardown of slot ms/lanes/slot-1")
            && said.contains("vivi pool destroy ms/lanes/slot-1"),
        "{said}"
    );
    assert!(
        !empty(&home.path().join("vivi-worker/.ledger")),
        "the slot is still there, and the error says so"
    );
}

#[test]
fn reconcile_destroys_a_dead_slot_that_no_dispatch_explains() {
    use crate::mailspace::Mailspace;
    use crate::pool::collect::Pacing;
    use crate::pool::pools::PoolSet;
    use crate::pool::slot::SlotStatus;

    let home = home("");
    let p = provider(home.path(), 2, "dir", 0);
    // What the failed dispatch left: made and seeded, never sealed ready,
    // boot ended, no claim.
    let mut made = spec("true\n");
    made.metadata.insert(META_SLOT.to_string(), "1".to_string());
    made.name = "ms-lanes-slot-1".to_string();
    let id = p.ssh.create(&made).unwrap();
    let started = Instant::now();
    while p.machine(&id).unwrap().state != "stopped" {
        assert!(started.elapsed() < Duration::from_secs(20), "seed did not end");
        std::thread::sleep(Duration::from_millis(50));
    }
    let pool = PoolId {
        mailspace: "ms".to_string(),
        pool: "lanes".to_string(),
    };
    let slots = |p: &WorkerProvider| -> Vec<SlotStatus> {
        crate::pool::run::pool_status(p, &pool).unwrap()
    };
    assert_eq!(slots(&p)[0].state, "unseeded");

    let tmp = tempfile::tempdir().unwrap();
    let space = Mailspace::init(Some(tmp.path())).unwrap();
    let set = PoolSet::single(&pool, &p);
    let pacing = Pacing::default();

    let young = crate::pool::work::reconcile(&space, &set, &pacing).unwrap();
    assert_eq!(
        slots(&p).len(),
        1,
        "a young slot may still be seeding: {:?}",
        young.out
    );

    let created = home.path().join("vivi-worker").join(&id).join("created");
    std::fs::write(&created, "2020-01-01T00:00:00Z\n").unwrap();
    let report = crate::pool::work::reconcile(&space, &set, &pacing).unwrap();
    assert!(
        report
            .out
            .iter()
            .any(|l| l.contains("orphan-stopped") && l.contains("ms-lanes-slot-1")),
        "{:?}",
        report.out
    );
    assert!(report.error().is_none(), "{:?}", report.error());
    assert!(p.list().unwrap().is_empty(), "the slot is destroyed");
    assert!(empty(&home.path().join("dirs")));
    assert!(empty(&home.path().join("vivi-worker/.ledger")));
}
