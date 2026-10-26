use super::super::model::RowState;
use super::{row, rows};

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
