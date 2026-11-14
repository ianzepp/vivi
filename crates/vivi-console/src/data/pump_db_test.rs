use vivi_core::pump::store::{DB_FILENAME, PumpStore};

use super::{PumpRead, pump_db_path};

fn project(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    std::fs::write(dir.path().join(".vivi/mailspace.toml"), config).unwrap();
    dir
}

#[test]
fn a_project_without_a_pump_has_no_pump_store() {
    let dir = project("name = \"demo\"\n");
    assert_eq!(pump_db_path(dir.path()).unwrap(), None);
    assert!(PumpRead::open(dir.path()).unwrap().is_none());
}

#[test]
fn the_pump_home_defaults_under_the_mailspace_and_honours_the_setting() {
    let dir = project("name = \"demo\"\n\n[pump]\n");
    assert_eq!(
        pump_db_path(dir.path()).unwrap(),
        Some(dir.path().join(".vivi/pump").join(DB_FILENAME))
    );
    let moved = project("name = \"demo\"\n\n[pump]\nhome = \"elsewhere\"\n");
    assert_eq!(
        pump_db_path(moved.path()).unwrap(),
        Some(moved.path().join("elsewhere").join(DB_FILENAME))
    );
}

#[test]
fn a_configured_pump_with_no_store_yet_opens_to_none() {
    let dir = project("name = \"demo\"\n\n[pump]\n");
    assert!(PumpRead::open(dir.path()).unwrap().is_none());
}

#[test]
fn an_existing_pump_store_reads_and_refuses_writes() {
    let dir = project("name = \"demo\"\n\n[pump]\n");
    let path = pump_db_path(dir.path()).unwrap().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(PumpStore::open(&path).unwrap());
    // A live pump keeps a connection open, and with it the WAL index.
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer
        .query_row("SELECT count(*) FROM units", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let before = std::fs::read(&path).unwrap();

    let read = PumpRead::open(dir.path()).unwrap().unwrap();
    let units: i64 = read
        .read(|conn| {
            conn.query_row("SELECT count(*) FROM units", [], |row| row.get(0))
                .map_err(|e| vivi_error::VivariumError::Other(e.to_string()))
        })
        .unwrap();
    assert_eq!(units, 0);
    let refused = read.read(|conn| {
        conn.execute("DELETE FROM units", [])
            .map_err(|e| vivi_error::VivariumError::Other(e.to_string()))
    });
    assert!(refused.is_err());
    drop(read);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(writer);
}

#[test]
fn a_wal_store_whose_writer_has_gone_still_reads_and_is_not_changed() {
    let dir = project("name = \"demo\"\n\n[pump]\n");
    let path = pump_db_path(dir.path()).unwrap().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(PumpStore::open(&path).unwrap());
    let before = std::fs::read(&path).unwrap();
    let read = PumpRead::open(dir.path()).unwrap().unwrap();
    let units: i64 = read
        .read(|conn| {
            conn.query_row("SELECT count(*) FROM units", [], |row| row.get(0))
                .map_err(|e| vivi_error::VivariumError::Other(e.to_string()))
        })
        .unwrap();
    assert_eq!(units, 0);
    drop(read);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_file_that_is_not_a_database_is_an_error() {
    let dir = project("name = \"demo\"\n\n[pump]\n");
    let path = pump_db_path(dir.path()).unwrap().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"not a database").unwrap();
    assert!(PumpRead::open(dir.path()).is_err());
}
