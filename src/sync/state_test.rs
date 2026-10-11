use super::*;

#[test]
fn a_missing_file_is_cursor_zero_and_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");

    let state = SyncState::load(&dir, "prj").unwrap();

    assert_eq!(state.since, 0);
    assert_eq!(state.project, "prj");
    assert!(!dir.exists());
}

#[test]
fn save_round_trips_and_leaves_no_temp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    let mut state = SyncState::load(&dir, "prj").unwrap();
    state.since = 42;
    state
        .dangling
        .insert("child".into(), "parent@example".into());

    state.save(&dir).unwrap();

    assert_eq!(SyncState::load(&dir, "prj").unwrap(), state);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [".gitignore", "state.json"]);
}

#[test]
fn the_sync_dir_ignores_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    SyncState::default().save(&dir).unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
        "*\n"
    );
}

#[test]
fn a_cursor_recorded_for_another_project_reads_as_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    let mut state = SyncState::load(&dir, "old").unwrap();
    state.since = 99;
    state.dangling.insert("c".into(), "p".into());
    state.save(&dir).unwrap();

    let other = SyncState::load(&dir, "new").unwrap();

    assert_eq!(other.since, 0);
    assert!(other.dangling.is_empty());
    assert_eq!(other.project, "new");
}

#[test]
fn an_unreadable_file_is_an_error_not_a_silent_reset() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("state.json"), "{not json").unwrap();

    assert!(SyncState::load(&dir, "prj").is_err());
}

#[test]
fn an_unreadable_file_names_itself_and_the_command_that_repairs_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("state.json"), "{\"project\":\"prj\",\"sinc").unwrap();

    let error = SyncState::load(&dir, "prj").unwrap_err().to_string();

    assert!(error.contains("state.json"), "{error}");
    assert!(error.contains("vivi sync resync"), "{error}");
    assert!(error.contains("is corrupt"), "{error}");
    assert!(!error.contains("EOF"), "no raw parser text: {error}");
}

#[test]
fn a_resync_starts_from_the_empty_state_when_the_file_is_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("state.json"), "").unwrap();

    let state = SyncState::load_for_resync(&dir, "prj");

    assert_eq!(state.project, "prj");
    assert_eq!(state.since, 0);
}
