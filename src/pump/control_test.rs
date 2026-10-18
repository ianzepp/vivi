//! Control mode migration and live-process compatibility tests.

use std::fs::{self, File};
use std::path::Path;

use tempfile::tempdir;

use crate::pump::control::{PumpMode, initialize, inspect, set_project_mode};
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

fn project_with_pump_home(home: &Path) -> tempfile::TempDir {
    let project = tempdir().unwrap();
    let mailspace = project.path().join(".vivi");
    fs::create_dir_all(&mailspace).unwrap();
    fs::write(
        mailspace.join("mailspace.toml"),
        format!(
            "name = \"control-test\"\n\n[pump]\nhome = \"{}\"\n",
            home.display()
        ),
    )
    .unwrap();
    project
}

#[test]
fn inspecting_control_does_not_migrate_an_older_schema() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    drop(PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(conn);

    assert_eq!(inspect(dir.path()).unwrap().mode, PumpMode::Running);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION - 1);
}

#[test]
fn stopping_a_live_old_schema_pump_uses_the_legacy_stop_flag() {
    let home_dir = tempdir().unwrap();
    let home = home_dir.path().join("pump");
    fs::create_dir_all(&home).unwrap();
    let project = project_with_pump_home(&home);
    let path = home.join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.initialize_control("running").unwrap();
    drop(store);
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    drop(conn);
    let lock = File::create(home.join("lock")).unwrap();
    lock.try_lock().unwrap();

    set_project_mode(project.path(), PumpMode::Stopped).unwrap();

    assert!(home.join("STOP").is_file());
    assert_eq!(inspect(&home).unwrap().mode, PumpMode::Stopped);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION - 1);
    drop(conn);
    drop(lock);

    assert_eq!(initialize(&home).unwrap().mode, PumpMode::Stopped);
    assert!(!home.join("STOP").exists());
}

#[test]
fn stopping_a_live_current_pump_writes_control_and_legacy_signal() {
    let home_dir = tempdir().unwrap();
    let home = home_dir.path().join("pump");
    fs::create_dir_all(&home).unwrap();
    let project = project_with_pump_home(&home);
    let path = home.join(DB_FILENAME);
    let mut store = PumpStore::open(&path).unwrap();
    store.initialize_control("running").unwrap();
    drop(store);
    let lock = File::create(home.join("lock")).unwrap();
    lock.try_lock().unwrap();

    set_project_mode(project.path(), PumpMode::Stopped).unwrap();

    assert!(home.join("STOP").is_file());
    assert_eq!(inspect(&home).unwrap().mode, PumpMode::Stopped);
    let store = PumpStore::open_read_only(&path).unwrap().unwrap();
    assert_eq!(store.control_mode().unwrap().as_deref(), Some("stopped"));
}
