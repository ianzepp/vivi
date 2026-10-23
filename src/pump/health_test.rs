//! Tests for [`crate::pump::health`]: verdict rules, the `/proc/locks`
//! writer probe, and a whole read over a saved store.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::tempdir;

use super::writer::{LockRow, ProcRoot, holder_in, parse_locks, split_dev};
use super::{HealthOptions, Signals, Verdict, decide, default_stale_after, health};
use crate::pump::state::{Attempt, NeedRecord, State, Status, Unit};
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
fn parked_work_and_stalls_each_give_a_reason() {
    let (verdict, reasons) = decide(&Signals {
        stuck: 2,
        quarantined: 1,
        needs_replan: 1,
        stall_exceptions: 1,
        ..running()
    });
    assert_eq!(verdict, Verdict::Degraded);
    assert_eq!(reasons.len(), 4, "{reasons:?}");
    assert!(reasons.contains(&String::from("2 units stuck")));
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

fn unit(need: &str, status: Status, since: u64) -> Unit {
    Unit {
        need: need.to_owned(),
        title: format!("{need} unit"),
        status,
        since,
        ..Unit::default()
    }
}

fn saved_state(now: u64) -> State {
    let mut st = State {
        merges: 1,
        ..State::default()
    };
    let mut merged = unit("n1", Status::Merged, now - 50);
    merged.commit = Some(String::from("abc1234"));
    merged.merged_at = Some(now - 50);
    st.units.insert(String::from("u1"), merged);
    let mut stuck = unit("n1", Status::Stuck, now - 7200);
    stuck.history.push(Attempt {
        why: String::from("finished/agent exit 1"),
        detail: String::from("proof failed"),
        ..Attempt::default()
    });
    st.units.insert(String::from("u2"), stuck);
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            units: vec![String::from("u1"), String::from("u2")],
            ..NeedRecord::default()
        },
    );
    st.needs.insert(
        String::from("n2"),
        NeedRecord {
            verdict: String::from("operator"),
            reason: String::from("needs a ruling"),
            ..NeedRecord::default()
        },
    );
    st
}

fn write_pump_home(home: &Path, now: u64) {
    fs::create_dir_all(home).unwrap();
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.save(&saved_state(now)).unwrap();
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
fn a_saved_store_reads_into_counts_merges_blocked_and_changes() {
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
    assert_eq!(h.verdict, Verdict::Degraded, "{:?}", h.reasons);
    assert!(
        h.reasons.contains(&String::from("1 units stuck")),
        "{:?}",
        h.reasons
    );
    assert_eq!(h.control.as_deref(), Some("running"));
    assert_eq!(h.units["merged"], 1);
    assert_eq!(h.units["stuck"], 1);
    assert_eq!(h.merges.total, 1);
    assert_eq!(h.merges.recent[0].commit.as_deref(), Some("abc1234"));
    assert_eq!(h.last_tick.as_ref().unwrap().age_secs, 2);
    assert_eq!(h.blocked.len(), 2);
    assert_eq!(
        (h.blocked[0].kind, h.blocked[0].id.as_str()),
        ("unit", "u2")
    );
    assert_eq!(h.blocked[0].reason, "finished/agent exit 1: proof failed");
    assert_eq!(h.blocked[0].age_secs, 7200);
    assert_eq!(
        (h.blocked[1].kind, h.blocked[1].state.as_str()),
        ("need", "operator")
    );
    let changed = h.changed_since.unwrap();
    assert_eq!(changed.merges, 1);
    assert_eq!(changed.units.len(), 1);
    assert_eq!(changed.units[0].unit, "u1");
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
