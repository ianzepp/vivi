//! Tests for `vivi pump reset`: the library archive half.

use std::fs::{self, File};
use std::path::Path;

use crate::pump::control::{self, PumpMode};
use crate::pump::fixture::project;
use crate::pump::reset::{archive, move_entries};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Every archived entry plus every keep-in-place one, with content in the
/// moved files and `plans/` so the rename of a directory is covered.
fn seed_home(home: &Path) {
    fs::create_dir_all(home).unwrap();
    for file in [
        "pump.db",
        "pump.db-wal",
        "pump.db-shm",
        "state.json",
        "state.json.imported",
        "reserved.txt",
        "exceptions.txt",
        "status.txt",
        "sync-push.log",
        "log.txt",
    ] {
        fs::write(home.join(file), "seed\n").unwrap();
    }
    for dir in ["plans", "resplit", "targets", "merge-lanes"] {
        fs::create_dir_all(home.join(dir)).unwrap();
    }
    fs::write(home.join("plans").join("need.json"), "{}\n").unwrap();
    fs::write(home.join("log.txt"), "old line\n").unwrap();
}

fn sorted_dir(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

const MOVED_ALL: [&str; 10] = [
    "exceptions.txt",
    "plans",
    "pump.db",
    "pump.db-shm",
    "pump.db-wal",
    "reserved.txt",
    "resplit",
    "state.json",
    "state.json.imported",
    "status.txt",
];

#[test]
fn archive_moves_the_listed_entries_and_keeps_targets_and_log() {
    let project = project();
    let home = project.home();
    seed_home(&home);

    let report = archive(project.root()).unwrap();

    assert!(report.archive_dir.starts_with(home.parent().unwrap()));
    assert!(
        report
            .archive_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pump-archive-"),
        "{}",
        report.archive_dir.display()
    );
    let mut moved = report.moved.clone();
    moved.sort();
    assert_eq!(moved, MOVED_ALL);
    for entry in MOVED_ALL {
        if entry == "pump.db" {
            // Reset leaves a fresh database; the seeded one is in the archive.
            assert!(home.join("pump.db").is_file(), "no fresh pump.db");
        } else {
            assert!(!home.join(entry).exists(), "{entry} stayed in the home");
        }
        assert!(
            report.archive_dir.join(entry).exists(),
            "{entry} not archived"
        );
    }
    assert!(report.archive_dir.join("plans").join("need.json").is_file());
    // SQLite may leave fresh -wal/-shm beside the new pump.db; everything
    // else in the home must be a keep-in-place entry.
    let allowed = [
        "log.txt",
        "lock",
        "merge-lanes",
        "pump.db",
        "pump.db-shm",
        "pump.db-wal",
        "sync-push.log",
        "targets",
    ];
    for name in sorted_dir(&home) {
        assert!(
            allowed.contains(&name.as_str()),
            "unexpected home entry {name}"
        );
    }
    for keep in ["targets", "merge-lanes", "sync-push.log", "lock", "log.txt"] {
        assert!(home.join(keep).exists(), "{keep} missing after reset");
    }
    let log = fs::read_to_string(home.join("log.txt")).unwrap();
    assert_eq!(
        log,
        format!(
            "old line\nreset: archived to {}\n",
            report.archive_dir.display()
        )
    );
}

#[test]
fn a_second_reset_gets_its_own_archive_and_keeps_both_pump_dbs() {
    let project = project();
    let home = project.home();
    seed_home(&home);
    fs::write(home.join("pump.db"), "first archive\n").unwrap();

    let first = archive(project.root()).unwrap();

    // The fresh database is what a same-second second reset would move;
    // give it its own bytes before archiving again.
    fs::write(home.join("pump.db"), "second archive\n").unwrap();

    let second = archive(project.root()).unwrap();

    assert_ne!(first.archive_dir, second.archive_dir);
    assert_eq!(
        fs::read_to_string(first.archive_dir.join("pump.db")).unwrap(),
        "first archive\n"
    );
    assert_eq!(
        fs::read_to_string(second.archive_dir.join("pump.db")).unwrap(),
        "second archive\n"
    );
}

#[test]
fn an_archive_destination_that_exists_is_an_error_not_a_replace() {
    let project = project();
    let home = project.home();
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("pump.db"), "home bytes\n").unwrap();
    let hand_made = home.parent().unwrap().join("pump-archive-hand-made");
    fs::create_dir_all(&hand_made).unwrap();
    fs::write(hand_made.join("pump.db"), "archived bytes\n").unwrap();

    let error = move_entries(&home, &hand_made).unwrap_err();

    assert!(error.to_string().contains("refusing to replace"), "{error}");
    assert_eq!(
        fs::read_to_string(hand_made.join("pump.db")).unwrap(),
        "archived bytes\n"
    );
    assert_eq!(
        fs::read_to_string(home.join("pump.db")).unwrap(),
        "home bytes\n"
    );
}

#[test]
fn a_held_lock_refuses_and_moves_nothing() {
    let project = project();
    let home = project.home();
    seed_home(&home);
    let lock = File::create(home.join("lock")).unwrap();
    lock.try_lock().unwrap();

    let error = archive(project.root()).unwrap_err();

    assert!(
        error.to_string().contains(
            "the pump is running (lock held): run vivi pump stop and wait for it to exit, then reset",
        ),
        "{error}"
    );
    assert_eq!(fs::read_to_string(home.join("pump.db")).unwrap(), "seed\n");
    assert!(home.join("status.txt").is_file());
    assert!(sorted_dir(&home).contains(&String::from("state.json")));
    assert_eq!(
        fs::read_to_string(home.join("log.txt")).unwrap(),
        "old line\n"
    );
    let archives: Vec<String> = fs::read_dir(home.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("pump-archive-"))
        .collect();
    assert!(archives.is_empty(), "{archives:?}");
}

#[test]
fn after_reset_control_reads_stopped_for_the_project() {
    let project = project();
    let home = project.home();
    fs::create_dir_all(&home).unwrap();
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.initialize_control("running").unwrap();
    drop(store);
    assert_eq!(control::inspect(&home).unwrap().mode, PumpMode::Running);

    let report = archive(project.root()).unwrap();

    assert_eq!(report.mode, PumpMode::Stopped);
    assert_eq!(control::inspect(&home).unwrap().mode, PumpMode::Stopped);
    // Opening the store left SQLite's -wal and -shm beside pump.db; all
    // three archive together.
    assert_eq!(
        report.moved,
        ["pump.db", "pump.db-wal", "pump.db-shm"].map(String::from)
    );
}

#[test]
fn the_report_counts_open_pump_records() {
    let project = project();
    let open_need = project.need("fix auth");
    project.mailspace.pump_send(None, &open_need, None).unwrap();
    let done_need = project.need("fix billing");
    let record = project.mailspace.pump_send(None, &done_need, None).unwrap();
    project.mailspace.pump_done(&record.handle, None).unwrap();

    let report = archive(project.root()).unwrap();

    assert_eq!(report.open_records, 1);
}
