use super::super::model::RowState;
use super::{row, rows};
use crate::pump::cycle::TickRecord;

#[test]
fn pump_header_points_to_log_when_present_and_is_idle_without_a_lock() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    std::fs::write(home.join("log.txt"), "tick\n").unwrap();
    let row = row("alpha", home, 1000).unwrap();
    assert_eq!(row.kind, super::super::model::RowKind::Pump);
    assert_eq!(row.state, RowState::Finished("idle".into()));
    assert_eq!(row.tail.as_deref(), Some(home.join("log.txt").as_path()));
}

#[test]
fn pump_tick_rows_show_timing_and_keep_finished_cycles_gray() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    crate::pump::cycle::begin(home, "tick-1".into(), 900).unwrap();
    let (active, warnings) = rows("alpha", home, 910);
    assert!(warnings.is_empty(), "{warnings:?}");
    let active_tick = active
        .iter()
        .find(|row| row.task.as_deref() == Some("tick-1"))
        .unwrap();
    assert_eq!(active_tick.state, RowState::Running);
    assert_eq!(active_tick.started_at, Some(900));
    assert_eq!(active_tick.running_secs, Some(10));
    assert_eq!(active_tick.finished_at, None);
    assert_eq!(active_tick.work_kind.as_deref(), Some("tick"));

    crate::pump::cycle::finish(home, "tick-1", 916, 16, "ok").unwrap();
    let (finished, warnings) = rows("alpha", home, 920);
    assert!(warnings.is_empty(), "{warnings:?}");
    let finished_tick = finished
        .iter()
        .find(|row| row.task.as_deref() == Some("tick-1"))
        .unwrap();
    assert_eq!(finished_tick.state, RowState::Finished("ok".into()));
    assert_eq!(finished_tick.started_at, Some(900));
    assert_eq!(finished_tick.running_secs, Some(16));
    assert_eq!(finished_tick.finished_at, Some(916));
    assert_eq!(finished_tick.stale_age_secs, None);

    let mut history = super::super::history::SessionHistory::default();
    history.refresh(finished, |_| None);
    history.refresh(Vec::new(), |_| None);
    let retained = history
        .rows()
        .iter()
        .find(|row| row.task.as_deref() == Some("tick-1"))
        .unwrap();
    assert_eq!(retained.state, RowState::Finished("ok".into()));
}

/// One snapshot over a project config's real monitor-cycles.jsonl whose
/// receipts straddle the shared recent-finished window: only the recorded
/// end decides expiry, whatever the start age or recorded duration say.
#[test]
fn old_finished_cycle_is_not_in_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("alpha");
    let config: crate::mailspace::MailspaceConfig =
        toml::from_str("name='alpha'\n[pump]\n").unwrap();
    let home = config.pump.as_ref().unwrap().home_dir(&root);
    std::fs::create_dir_all(&home).unwrap();
    let now = 1_000_000_u64;
    let tick = |id: &str, started_at: u64, ended_at: Option<u64>, duration_secs: Option<u64>| {
        TickRecord {
            id: id.to_owned(),
            started_at,
            ended_at,
            duration_secs,
            outcome: ended_at.map(|_| "ok".to_owned()),
        }
    };
    let records = vec![
        // Finished a day ago: expired by the recorded end.
        tick("day-old", now - 86410, Some(now - 86400), Some(10)),
        // Start plus duration lands inside the window but the recorded end
        // does not: end-based expiry must drop it anyway.
        tick("bridged", now - 400, Some(now - 350), Some(380)),
        // Exactly on the window boundary: inclusive, so it stays.
        tick("boundary", now - 310, Some(now - 300), Some(10)),
        // Recently finished.
        tick("recent", now - 70, Some(now - 60), Some(10)),
        // Old start, completing now: recent by its end, and the recorded
        // duration differs from end minus start yet is preserved verbatim.
        tick("late", now - 5000, Some(now), Some(123)),
        // Still active: visible whatever its start age, with no completion.
        tick("old-active", now - 5000, None, None),
    ];
    let mut text = String::new();
    for record in &records {
        text.push_str(&serde_json::to_string(record).unwrap());
        text.push('\n');
    }
    std::fs::write(home.join("monitor-cycles.jsonl"), text).unwrap();

    let (rows, warnings) = rows("alpha", &home, now);
    assert!(warnings.is_empty(), "{warnings:?}");
    let by_id = |name: &str| {
        rows.iter()
            .find(|row| row.task.as_deref() == Some(name))
            .cloned()
    };
    assert!(by_id("day-old").is_none(), "day-old finish must expire");
    assert!(
        by_id("bridged").is_none(),
        "expiry must follow the recorded end, not start plus duration"
    );
    let boundary = by_id("boundary").unwrap();
    assert_eq!(boundary.state, RowState::Finished("ok".into()));
    assert_eq!(boundary.finished_at, Some(now - 300));
    let recent = by_id("recent").unwrap();
    assert_eq!(recent.state, RowState::Finished("ok".into()));
    assert_eq!(recent.finished_at, Some(now - 60));
    assert_eq!(recent.stale_age_secs, None);
    assert_eq!(recent.running_secs, Some(10));
    let late = by_id("late").unwrap();
    assert_eq!(late.state, RowState::Finished("ok".into()));
    assert_eq!(late.finished_at, Some(now));
    assert_eq!(late.running_secs, Some(123));
    let old_active = by_id("old-active").unwrap();
    assert_eq!(old_active.state, RowState::Running);
    assert_eq!(old_active.finished_at, None);
    assert_eq!(old_active.started_at, Some(now - 5000));
    assert_eq!(old_active.running_secs, Some(5000));
    // The persistent header is a status summary, not historical activity:
    // it survives beside whatever cycles expired.
    let header = rows.iter().find(|row| row.task.is_none()).unwrap();
    assert_eq!(header.state, RowState::Finished("idle".into()));
    assert_eq!(header.finished_at, None);
}

/// Byte length of the truncated header used for the unreadable-input case.
const SHORT_HEADER_LEN: usize = 10;

fn sidecar_paths(home: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        home.join(format!("{}-wal", crate::pump::store::DB_FILENAME)),
        home.join(format!("{}-shm", crate::pump::store::DB_FILENAME)),
    )
}

/// Copy a quiescent pump database and its committed WAL into `home` without
/// the writer's `-shm` index. The recently committed control mode exists
/// only in the copied WAL; no live writer file is ever unlinked.
fn copy_owner_wal_fixture(home: &std::path::Path) {
    let source = tempfile::tempdir().unwrap();
    let source_db = source.path().join(crate::pump::store::DB_FILENAME);
    let mut store = crate::pump::store::PumpStore::open(&source_db).unwrap();
    store.initialize_control("running").unwrap();
    // Checkpoint the initialization so the mode commit below lives only in
    // the WAL, then commit it while the writer stays open and quiescent.
    let checkpointer = rusqlite::Connection::open(&source_db).unwrap();
    checkpointer
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(checkpointer);
    store.set_control_mode("paused").unwrap();
    let wal_name = format!("{}-wal", crate::pump::store::DB_FILENAME);
    std::fs::copy(&source_db, home.join(crate::pump::store::DB_FILENAME)).unwrap();
    std::fs::copy(source.path().join(&wal_name), home.join(&wal_name)).unwrap();
    drop(store);
    // The copied database alone must still report the pre-commit mode: the
    // paused commit is recoverable only beside the copied WAL.
    let check = tempfile::tempdir().unwrap();
    std::fs::copy(
        home.join(crate::pump::store::DB_FILENAME),
        check.path().join(crate::pump::store::DB_FILENAME),
    )
    .unwrap();
    let alone =
        rusqlite::Connection::open(check.path().join(crate::pump::store::DB_FILENAME)).unwrap();
    let mode: String = alone
        .query_row(
            "SELECT mode FROM pump_control WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        mode, "running",
        "the paused commit must live only in the copied WAL"
    );
}

#[test]
fn owner_monitor_reads_a_wal_only_control_value_without_touching_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    copy_owner_wal_fixture(home);
    let db = home.join(crate::pump::store::DB_FILENAME);
    let (wal, _shm) = sidecar_paths(home);
    assert!(!_shm.exists(), "the fixture must be copied without -shm");
    let before_db = std::fs::read(&db).unwrap();
    let before_wal = std::fs::read(&wal).unwrap();

    let row = row("alpha", home, 1000).unwrap();
    assert_eq!(row.kind, super::super::model::RowKind::Pump);
    assert_eq!(row.role, "paused");
    assert_eq!(row.state, RowState::Finished("paused".into()));
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before_db,
        "database bytes must not change"
    );
    assert_eq!(
        std::fs::read(&wal).unwrap(),
        before_wal,
        "WAL bytes must not change"
    );
    assert!(_shm.exists(), "the owner read may create its own -shm");
}

#[test]
fn absent_pump_db_stays_optional_without_creating_database_or_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let row = row("alpha", home, 1000).unwrap();
    assert_eq!(row.state, RowState::Finished("idle".into()));
    let db = home.join(crate::pump::store::DB_FILENAME);
    assert!(!db.exists(), "the monitor must not create the database");
    let (wal, shm) = sidecar_paths(home);
    assert!(!wal.exists(), "the monitor must not create a WAL");
    assert!(!shm.exists(), "the monitor must not create a sidecar");
}

#[test]
fn non_file_pump_db_is_unavailable_without_creating_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let db = home.join(crate::pump::store::DB_FILENAME);
    std::fs::create_dir(&db).unwrap();
    let error = row("alpha", home, 1000).err().unwrap().to_string();
    assert!(error.contains("not a regular database file"), "{error}");
    let (wal, shm) = sidecar_paths(home);
    assert!(!wal.exists(), "the monitor must not create a WAL");
    assert!(!shm.exists(), "the monitor must not create a sidecar");
}

#[test]
fn unreadable_pump_db_is_unavailable_without_creating_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let db = home.join(crate::pump::store::DB_FILENAME);
    std::fs::write(&db, vec![0_u8; SHORT_HEADER_LEN]).unwrap();
    let before = std::fs::read(&db).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let error = row("alpha", home, 1000).err().unwrap().to_string();
    assert!(!error.is_empty());
    let (wal, shm) = sidecar_paths(home);
    assert!(!wal.exists(), "the monitor must not create a WAL");
    assert!(!shm.exists(), "the monitor must not create a sidecar");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert_eq!(std::fs::read(&db).unwrap(), before, "bytes must not change");
}
