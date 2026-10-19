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

#[test]
fn monitor_skips_wal_pump_db_without_creating_its_shm_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let db = home.join(crate::pump::store::DB_FILENAME);
    drop(crate::pump::store::PumpStore::open(&db).unwrap());
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(connection);
    let shm = home.join(format!("{}-shm", crate::pump::store::DB_FILENAME));
    let wal = home.join(format!("{}-wal", crate::pump::store::DB_FILENAME));
    let _ = std::fs::remove_file(&shm);
    let _ = std::fs::remove_file(&wal);
    let before = std::fs::read(&db).unwrap();

    let error = row("alpha", home, 1000).err().unwrap().to_string();
    assert!(error.contains("pump.db-shm"), "{error}");
    assert!(!shm.exists(), "the monitor created the missing sidecar");
    assert_eq!(std::fs::read(db).unwrap(), before);
}
