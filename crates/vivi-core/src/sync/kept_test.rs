//! Store tests for the kept-local list in `kept-local.json`.

use super::*;

#[test]
fn a_missing_file_is_an_empty_list_that_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");

    let kept = KeptLocal::load(&dir).unwrap();

    assert!(kept.is_empty());
    assert_eq!(kept.len(), 0);
    assert!(!kept.contains(&"00".repeat(32)));
    assert!(!dir.exists());
}

#[test]
fn save_round_trips_sorted_and_leaves_no_temp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    let mut kept = KeptLocal::load(&dir).unwrap();
    kept.insert("ff".repeat(32));
    kept.insert("00".repeat(32));
    kept.insert("77".repeat(32));

    kept.save(&dir).unwrap();

    let kept = KeptLocal::load(&dir).unwrap();
    assert_eq!(kept.len(), 3);
    assert!(kept.contains(&"00".repeat(32)));
    assert!(kept.contains(&"77".repeat(32)));
    assert!(kept.contains(&"ff".repeat(32)));
    let raw = std::fs::read_to_string(dir.join("kept-local.json")).unwrap();
    assert!(
        raw.starts_with("[\n  \"0000"),
        "a sorted JSON array, smallest id first: {raw}"
    );
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [".gitignore", "kept-local.json"]);
}

#[test]
fn save_creates_the_sync_dir_which_ignores_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");

    KeptLocal::default().save(&dir).unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.join(".gitignore")).unwrap(),
        "*\n"
    );
    assert_eq!(KeptLocal::load(&dir).unwrap().len(), 0);
}

#[test]
fn a_corrupt_file_is_an_error_naming_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("kept-local.json"), "[\"incomplete").unwrap();

    let error = KeptLocal::load(&dir).unwrap_err().to_string();

    assert!(error.contains("kept-local.json"), "{error}");
    assert!(error.contains("is corrupt"), "{error}");
    assert!(!error.contains("EOF"), "no raw parser text: {error}");
}

#[test]
fn an_unreadable_file_is_an_error_naming_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(dir.join("kept-local.json")).unwrap();

    let error = KeptLocal::load(&dir).unwrap_err().to_string();

    assert!(error.contains("kept-local.json"), "{error}");
    assert!(error.contains("cannot be read"), "{error}");
}
