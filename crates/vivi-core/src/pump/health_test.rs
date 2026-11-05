//! Tests for [`crate::pump::health`]: verdict rules, the `/proc/locks`
//! writer probe, and a whole read over a saved store.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::tempdir;

use super::writer::{LockRow, ProcRoot, holder_in, parse_locks, split_dev};
use super::{HealthOptions, Signals, Verdict, decide, default_stale_after, health};
use crate::pump::report::{self, PumpRead};
use crate::pump::state::State;
use crate::pump::store::build_waits::{BuildPrerequisite, BuildWait, PrerequisiteState};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn running() -> Signals {
    Signals {
        control: Some(String::from("running")),
        writer_running: Some(true),
        tick_age: Some(5),
        status_age: Some(5),
        stale_after: 600,
        ..Signals::default()
    }
}

#[test]
fn a_fresh_running_pump_is_ok() {
    assert_eq!(decide(&running()), (Verdict::Ok, Vec::new()));
}

#[test]
fn a_running_writer_with_a_stale_tick_is_stalled() {
    let (verdict, reasons) = decide(&Signals {
        tick_age: Some(601),
        ..running()
    });
    assert_eq!(verdict, Verdict::Stalled);
    assert!(reasons[0].contains("last tick 601s ago"), "{reasons:?}");
}

#[test]
fn a_running_writer_that_never_ticked_is_stalled() {
    let (verdict, _) = decide(&Signals {
        tick_age: None,
        ..running()
    });
    assert_eq!(verdict, Verdict::Stalled);
}

#[test]
fn no_writer_with_an_old_status_is_stalled() {
    let (verdict, reasons) = decide(&Signals {
        writer_running: Some(false),
        status_age: Some(4000),
        ..running()
    });
    assert_eq!(verdict, Verdict::Stalled);
    assert!(reasons[0].contains("no writer"), "{reasons:?}");
}

#[test]
fn no_writer_with_a_fresh_status_is_degraded() {
    let (verdict, reasons) = decide(&Signals {
        writer_running: Some(false),
        status_age: Some(30),
        ..running()
    });
    assert_eq!(verdict, Verdict::Degraded);
    assert!(reasons[0].contains("may be restarting"), "{reasons:?}");
}

#[test]
fn a_paused_pump_is_degraded_never_stalled() {
    let (verdict, reasons) = decide(&Signals {
        control: Some(String::from("paused")),
        writer_running: Some(false),
        tick_age: Some(9000),
        status_age: Some(9000),
        ..running()
    });
    assert_eq!(verdict, Verdict::Degraded);
    assert_eq!(reasons, vec![String::from("control paused")]);
}

#[test]
fn stale_after_defaults_to_thirty_ticks_with_a_ten_minute_floor() {
    assert_eq!(default_stale_after(20), 600);
    assert_eq!(default_stale_after(60), 1800);
}

const LOCKS: &str = "\
1: POSIX  ADVISORY  WRITE 77 08:01:500 0 EOF
2: FLOCK  ADVISORY  WRITE 1234 08:01:131074 0 EOF
2: -> FLOCK  ADVISORY  WRITE 999 08:01:131074 0 EOF
3: FLOCK  ADVISORY  WRITE 42 fd:02:9 0 EOF
";

#[test]
fn proc_locks_rows_skip_posix_locks_and_waiters() {
    assert_eq!(
        parse_locks(LOCKS),
        vec![
            LockRow {
                pid: 1234,
                major: 8,
                minor: 1,
                inode: 131_074
            },
            LockRow {
                pid: 42,
                major: 0xfd,
                minor: 2,
                inode: 9
            },
        ]
    );
}

#[test]
fn linux_dev_numbers_split_like_glibc() {
    assert_eq!(split_dev(0x801), (8, 1));
    assert_eq!(split_dev(0xfd02), (0xfd, 2));
    // major 259, minor 300: high minor bits live above bit 20.
    let dev = (300 & 0xff) | ((259 & 0xfff) << 8) | ((300 & !0xff) << 12);
    assert_eq!(split_dev(dev), (259, 300));
}

#[cfg(unix)]
fn locks_row_for(path: &Path, pid: u32) -> String {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).unwrap();
    let (major, minor) = split_dev(meta.dev());
    format!(
        "1: FLOCK  ADVISORY  WRITE {pid} {major:02x}:{minor:02x}:{} 0 EOF\n",
        meta.ino()
    )
}

#[cfg(unix)]
#[test]
fn the_holder_comes_from_proc_locks_without_touching_the_lock() {
    let dir = tempdir().unwrap();
    let lock = dir.path().join("lock");
    fs::write(&lock, "").unwrap();
    let locks = dir.path().join("locks");
    let root = ProcRoot {
        locks: &locks,
        proc_dir: dir.path(),
    };
    assert_eq!(holder_in(&lock, &root), None, "no /proc/locks: not Linux");
    fs::write(&locks, LOCKS).unwrap();
    assert_eq!(holder_in(&lock, &root), Some(None));
    fs::write(&locks, format!("{LOCKS}{}", locks_row_for(&lock, 4321))).unwrap();
    assert_eq!(holder_in(&lock, &root), Some(Some(4321)));
    let missing = dir.path().join("never-created");
    assert_eq!(holder_in(&missing, &root), Some(None));
}

#[cfg(unix)]
#[test]
fn an_inode_match_on_another_device_needs_the_holder_to_have_it_open() {
    use std::os::unix::fs::{MetadataExt, symlink};
    let dir = tempdir().unwrap();
    let lock = dir.path().join("lock");
    fs::write(&lock, "").unwrap();
    let inode = fs::metadata(&lock).unwrap().ino();
    let locks = dir.path().join("locks");
    fs::write(
        &locks,
        format!("1: FLOCK ADVISORY WRITE 55 ff:ff:{inode} 0 EOF\n"),
    )
    .unwrap();
    let root = ProcRoot {
        locks: &locks,
        proc_dir: dir.path(),
    };
    assert_eq!(holder_in(&lock, &root), Some(None));
    let fd = dir.path().join("55").join("fd");
    fs::create_dir_all(&fd).unwrap();
    symlink(fs::canonicalize(&lock).unwrap(), fd.join("3")).unwrap();
    assert_eq!(holder_in(&lock, &root), Some(Some(55)));
}

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn project(toml: &str) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(dir.path().join(".vivi").join("mailspace.toml"), toml).unwrap();
    dir
}

const TOML: &str = "name = \"p\"\n\n[pump]\ntick = 20\n";

#[test]
fn a_project_without_a_pump_table_is_unknown() {
    let dir = project("name = \"p\"\n");
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    assert_eq!(h.verdict, Verdict::Unknown);
    assert!(h.reasons[0].contains("no [pump] table"), "{:?}", h.reasons);
}

#[test]
fn a_pump_without_a_database_is_unknown_and_creates_nothing() {
    let dir = project(TOML);
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    assert_eq!(h.verdict, Verdict::Unknown);
    assert_eq!(h.tick_secs, 20);
    assert_eq!(h.stale_after_secs, 600);
    assert!(h.reasons[0].contains("no pump.db"), "{:?}", h.reasons);
    assert!(!dir.path().join(".vivi/pump").exists());
}

fn write_pump_home(home: &Path, now: u64) {
    fs::create_dir_all(home).unwrap();
    let store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();
    store.set_control_mode("running").unwrap();
    drop(store);
    let tick = format!(
        "{{\"id\":\"t1\",\"started_at\":{},\"ended_at\":{},\"duration_secs\":1,\"outcome\":\"ok\"}}\n",
        now - 3,
        now - 2
    );
    fs::write(home.join("monitor-cycles.jsonl"), tick).unwrap();
    fs::write(home.join("status.txt"), "status\n").unwrap();
}

#[test]
fn a_saved_store_reads_into_control_tick_and_exceptions() {
    let dir = project(TOML);
    let now = epoch_now();
    let home = dir.path().join(".vivi/pump");
    write_pump_home(&home, now);
    let opts = HealthOptions {
        since: Some(now - 100),
        now: Some(now),
        ..HealthOptions::default()
    };
    let h = health(dir.path(), &opts).unwrap();
    assert_eq!(h.control.as_deref(), Some("running"));
    assert_eq!(h.last_tick.as_ref().unwrap().age_secs, 2);
    assert!(h.exceptions.is_empty());
    assert_eq!(h.changed_since.unwrap().exceptions, 0);
}

#[test]
fn the_json_shape_names_the_verdict_in_lowercase() {
    let dir = project(TOML);
    let now = epoch_now();
    write_pump_home(&dir.path().join(".vivi/pump"), now);
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    let value = serde_json::to_value(&h).unwrap();
    assert_eq!(value["verdict"], "degraded");
    assert!(value.get("changed_since").is_none());
    for key in ["units", "merges", "blocked", "complete_open_needs"] {
        assert!(value.get(key).is_none(), "{key}");
    }
    assert!(value["writer"]["source"].is_string());
    assert!(super::render(&h).starts_with("verdict  degraded\n"));
}

fn fresh_initialised_project() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let mut space = crate::mailspace::Mailspace::init(Some(dir.path())).unwrap();
    space.ensure_operator_role().unwrap();
    space.ensure_merge_role().unwrap();
    space.ensure_pump_planner_role().unwrap();
    space.ensure_starter_pump().unwrap();
    crate::pump::control::set_project_mode(dir.path(), crate::pump::control::PumpMode::Running)
        .unwrap();
    dir
}

#[test]
fn a_fresh_project_reports_not_ready_with_its_missing_items() {
    let dir = fresh_initialised_project();
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    assert_eq!(h.verdict, Verdict::Unknown);
    let reasons = h.reasons.join("\n");
    assert!(reasons.contains("not ready: "), "{reasons}");
    assert!(reasons.contains("pump.committer_name"), "{reasons}");
}

#[test]
fn a_holding_pump_is_unknown_not_stalled() {
    let dir = fresh_initialised_project();
    crate::pump::run::run(dir.path(), true).unwrap();
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    assert_eq!(h.verdict, Verdict::Unknown, "{:?}", h.reasons);
    assert_eq!(h.reasons.len(), 1, "{:?}", h.reasons);
    assert!(h.reasons[0].starts_with("not ready: "), "{:?}", h.reasons);
    assert_eq!(h.control.as_deref(), Some("running"));
}

fn wait(need: &str, record: &str, prerequisites: &[(&str, PrerequisiteState)]) -> BuildWait {
    BuildWait {
        need: need.into(),
        record: record.into(),
        prerequisites: prerequisites
            .iter()
            .map(|(handle, state)| BuildPrerequisite {
                handle: (*handle).into(),
                state: *state,
                detail: None,
                outcome: None,
            })
            .collect(),
        observed_at: 1_700_000_000,
    }
}

/// A pump home holding `waits`, with a fresh tick and a status file.
fn home_with_waits(now: u64, waits: &[BuildWait]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = project(TOML);
    let home = dir.path().join(".vivi/pump");
    write_pump_home(&home, now);
    let store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    for wait in waits {
        store.upsert_build_wait(wait).unwrap();
    }
    drop(store);
    (dir, home)
}

#[test]
fn waiting_carries_the_persisted_handles_states_and_details() {
    let now = epoch_now();
    let mut first = wait(
        "n1",
        "r1",
        &[
            ("b", PrerequisiteState::ClosedUnlanded),
            ("a", PrerequisiteState::Held),
        ],
    );
    first.prerequisites[0].outcome = Some(String::from("failed"));
    first.prerequisites[1].detail = Some(String::from("waiting on the gate"));
    let second = wait("n2", "r2", &[("c", PrerequisiteState::Open)]);
    let (dir, home) = home_with_waits(now, &[first, second]);
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    assert_eq!(h.waiting.len(), 2, "{:?}", h.waiting);
    let view = &h.waiting[0];
    assert_eq!(view.need, "n1");
    assert_eq!(view.record, "r1");
    assert_eq!(view.observed_at, 1_700_000_000);
    let handles: Vec<&str> = view
        .prerequisites
        .iter()
        .map(|prerequisite| prerequisite.handle.as_str())
        .collect();
    assert_eq!(handles, ["a", "b"], "a wait renders in handle order");
    assert_eq!(view.prerequisites[0].state, PrerequisiteState::Held);
    assert_eq!(
        view.prerequisites[0].detail.as_deref(),
        Some("waiting on the gate")
    );
    assert_eq!(
        view.prerequisites[1].state,
        PrerequisiteState::ClosedUnlanded
    );
    assert_eq!(view.prerequisites[1].outcome.as_deref(), Some("failed"));
    assert_eq!(h.waiting[1].prerequisites[0].state, PrerequisiteState::Open);
    // The same handles and states as the persisted projection, not a
    // classification recomputed here.
    let PumpRead::Ready(snapshot) = report::read(&home).unwrap() else {
        panic!("expected a ready snapshot");
    };
    assert_eq!(snapshot.build_waits().len(), h.waiting.len());
    for (persisted, view) in snapshot.build_waits().iter().zip(&h.waiting) {
        assert_eq!(&persisted.need, &view.need);
        assert_eq!(&persisted.record, &view.record);
        assert_eq!(persisted.observed_at, view.observed_at);
        // The view orders the prerequisites; the persisted row keeps insert
        // order, so match by handle rather than by position.
        for view in &view.prerequisites {
            let prerequisite = persisted
                .prerequisites
                .iter()
                .find(|prerequisite| prerequisite.handle == view.handle)
                .unwrap_or_else(|| panic!("{} is not persisted", view.handle));
            assert_eq!(prerequisite.state, view.state);
            assert_eq!(prerequisite.detail, view.detail);
            assert_eq!(prerequisite.outcome, view.outcome);
        }
    }
    // The JSON names each classification in its persisted snake case.
    let value = serde_json::to_value(&h).unwrap();
    assert_eq!(value["waiting"][0]["record"], "r1");
    assert_eq!(value["waiting"][0]["observed_at"], 1_700_000_000);
    assert_eq!(value["waiting"][0]["prerequisites"][0]["state"], "held");
    assert_eq!(
        value["waiting"][0]["prerequisites"][1]["state"],
        "closed_unlanded"
    );
    assert!(
        value["waiting"][1]["prerequisites"][0]
            .get("detail")
            .is_none()
    );
}

#[test]
fn waiting_reads_are_deterministic_and_ignore_the_exception_window() {
    let now = epoch_now();
    let (dir, home) = home_with_waits(now, &[wait("n1", "r1", &[("a", PrerequisiteState::Held)])]);
    let opts = HealthOptions {
        since: Some(now + 10_000),
        now: Some(now),
        ..HealthOptions::default()
    };
    let first = health(dir.path(), &opts).unwrap();
    let second = health(dir.path(), &opts).unwrap();
    // Current snapshot data: a cursor past the observation keeps the wait.
    assert_eq!(first.waiting.len(), 1, "waiting is not windowed");
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
    assert_eq!(super::render(&first), super::render(&second));
    let text = super::render(&first);
    assert!(
        text.contains("waiting  1\n  need n1 record r1 observed 1700000000 waiting on a held\n"),
        "{text}"
    );
    // Nothing created: no lock taken, no file written by a read.
    let lock = home.join("lock").exists();
    let before = listing(&home);
    drop(health(dir.path(), &opts).unwrap());
    drop(health(dir.path(), &opts).unwrap());
    assert_eq!(listing(&home), before, "a read wrote under the pump home");
    assert_eq!(
        home.join("lock").exists(),
        lock,
        "a read took the pump lock"
    );
}

#[test]
fn a_database_without_the_waits_table_has_no_waiting() {
    let now = epoch_now();
    let (dir, home) = home_with_waits(now, &[]);
    let conn = rusqlite::Connection::open(home.join(DB_FILENAME)).unwrap();
    conn.execute_batch("DROP TABLE build_waits").unwrap();
    drop(conn);
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    assert!(h.waiting.is_empty(), "{:?}", h.waiting);
    let value = serde_json::to_value(&h).unwrap();
    assert_eq!(value["waiting"], serde_json::json!([]));
    let conn = rusqlite::Connection::open(home.join(DB_FILENAME)).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'build_waits'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "a health read must not create the table");
}

#[test]
fn a_wait_leaves_health_after_its_admission_is_recorded() {
    let now = epoch_now();
    let (dir, home) = home_with_waits(now, &[wait("n1", "r1", &[("a", PrerequisiteState::Held)])]);
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    assert_eq!(h.waiting.len(), 1);
    let store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.remove_build_wait("n1").unwrap();
    drop(store);
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    assert!(h.waiting.is_empty(), "{:?}", h.waiting);
}

#[test]
fn ordinary_waiting_does_not_change_the_verdict() {
    let now = epoch_now();
    let (plain, _home) = home_with_waits(now, &[]);
    let (waiting, _home) = home_with_waits(
        now,
        &[
            wait(
                "n1",
                "r1",
                &[
                    ("a", PrerequisiteState::Held),
                    ("b", PrerequisiteState::Open),
                ],
            ),
            wait("n2", "r2", &[("c", PrerequisiteState::ClosedUnlanded)]),
        ],
    );
    let opts = HealthOptions {
        now: Some(now),
        ..HealthOptions::default()
    };
    let plain = health(plain.path(), &opts).unwrap();
    let waiting = health(waiting.path(), &opts).unwrap();
    assert_eq!(waiting.waiting.len(), 2);
    assert_eq!(waiting.verdict, plain.verdict, "{:?}", waiting.reasons);
    assert_eq!(waiting.reasons, plain.reasons);
    assert_eq!(waiting.schema, plain.schema);
}

#[test]
fn waiting_agrees_with_the_status_surface() {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(
        dir.path().join(".vivi").join("mailspace.toml"),
        format!(
            "name = \"p\"\n\n[pump]\ntick = 20\nhome = \"{}\"\n",
            dir.path().join("home").display()
        ),
    )
    .unwrap();
    let (ctx, _host) =
        crate::pump::host::test_ctx(dir.path(), crate::pump::config::PumpConfig::default());
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();
    let mut first = wait(
        "n1",
        "r1",
        &[
            ("b", PrerequisiteState::ClosedUnlanded),
            ("a", PrerequisiteState::Held),
        ],
    );
    first.prerequisites[0].outcome = Some(String::from("failed"));
    first.prerequisites[1].detail = Some(String::from("waiting on the gate"));
    store.upsert_build_wait(&first).unwrap();
    drop(store);
    crate::pump::run::write_status(&ctx, &State::default());
    let status = fs::read_to_string(ctx.home.join("status.txt")).unwrap();
    let h = health(dir.path(), &HealthOptions::default()).unwrap();
    let rendered = super::render(&h);
    let view = &h.waiting[0];
    let status_line = status
        .lines()
        .find(|line| line.starts_with(&format!("need {} waiting on ", view.need)))
        .unwrap_or_else(|| panic!("no status line for {}: {status}", view.need));
    let on = status_line.split_once("waiting on ").unwrap().1;
    let rendered_line = rendered
        .lines()
        .find(|line| line.contains(&format!("need {} record {}", view.need, view.record)))
        .unwrap_or_else(|| panic!("no health line for {}: {rendered}", view.need));
    assert!(
        rendered_line.ends_with(&format!("waiting on {on}")),
        "{rendered_line} vs {status_line}"
    );
}

/// Every entry of a directory with its length, sorted, for read-only checks.
fn listing(dir: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let len = entry.metadata().unwrap().len();
            (entry.file_name().to_string_lossy().into_owned(), len)
        })
        .filter(|(name, _)| !name.ends_with("-shm") && !name.ends_with("-wal"))
        .collect();
    out.sort();
    out
}
