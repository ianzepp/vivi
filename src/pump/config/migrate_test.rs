//! Tests for `pump::config::migrate`.

use std::fs;
use std::path::Path;

use tempfile::tempdir;

use super::migrate;
use crate::mailspace::Mailspace;

fn init() -> tempfile::TempDir {
    let tmp = tempdir().unwrap();
    Mailspace::init_with_defaults(Some(tmp.path())).unwrap();
    tmp
}

fn config_path(root: &Path) -> std::path::PathBuf {
    root.join(".vivi/mailspace.toml")
}

fn append(root: &Path, text: &str) {
    let path = config_path(root);
    let mut raw = fs::read_to_string(&path).unwrap();
    raw.push_str(text);
    fs::write(path, raw).unwrap();
}

#[test]
fn dry_run_names_a_retired_key_and_does_not_write() {
    let tmp = init();
    append(tmp.path(), "branch = \"old\"\n");
    let path = config_path(tmp.path());
    let before = fs::read_to_string(&path).unwrap();
    let report = migrate(Some(tmp.path()), false).unwrap();
    assert!(report.dry_run);
    assert!(report.config_backup.is_none());
    assert!(report.wrote.is_none());
    assert!(report.charters.is_empty());
    assert!(before.contains("branch = \"old\""), "{before}");
    assert!(
        report.plan.contains("remove [pump] branch = \"old\""),
        "{}",
        report.plan
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn write_removes_a_retired_key_and_keeps_the_backup() {
    let tmp = init();
    append(tmp.path(), "branch = \"old\"\n");
    let path = config_path(tmp.path());
    let before = fs::read_to_string(&path).unwrap();
    assert!(before.contains("branch = \"old\""), "{before}");
    let report = migrate(Some(tmp.path()), true).unwrap();
    assert!(!report.dry_run);
    let after = fs::read_to_string(&path).unwrap();
    assert!(!after.contains("branch"), "{after}");
    let backup = report.config_backup.expect("config backup");
    assert_eq!(fs::read_to_string(&backup).unwrap(), before);
    let name = backup.file_name().unwrap().to_string_lossy();
    assert!(name.starts_with("mailspace.toml.bak."), "{name}");
    assert_eq!(report.wrote.as_deref(), Some(path.as_path()));
}

#[test]
fn write_replaces_a_stale_charter_and_backs_it_up() {
    let tmp = init();
    let charter = tmp.path().join(".vivi/charters/merge.md");
    assert!(charter.is_file(), "init writes the merge charter");
    let stale = "reply in the old way\n";
    fs::write(&charter, stale).unwrap();
    let report = migrate(Some(tmp.path()), true).unwrap();
    let written = report
        .charters
        .iter()
        .find(|row| row.role == "merge")
        .unwrap_or_else(|| panic!("{:?}", report.charters));
    let text = fs::read_to_string(&charter).unwrap();
    assert!(text.lines().any(|line| line.trim() == "+++"), "{text}");
    assert_eq!(written.bytes, text.len());
    let backup = written.backup.as_ref().expect("charter backup");
    assert_eq!(fs::read_to_string(backup).unwrap(), stale);
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("merge.md.bak."),
        "{}",
        backup.display()
    );
}
