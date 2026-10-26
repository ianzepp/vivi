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

/// A project under a base config, so a migrate can prove it touches only the
/// selected project file. Returns `(guard, project root, base config path)`.
fn init_inherited() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempdir().unwrap();
    let base_file = tmp.path().join(".vivi/mailspace.toml");
    fs::create_dir_all(base_file.parent().unwrap()).unwrap();
    fs::write(
        &base_file,
        "base = true\n\n[pump]\nbranch = \"base-branch\"\n",
    )
    .unwrap();
    let project = tmp.path().join("proj");
    Mailspace::init_with_defaults(Some(&project)).unwrap();
    (tmp, project, base_file)
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

/// The pump identity keys are the five sender/recipient knobs the catalogue
/// schedules for removal. Migration edits only the selected project file:
/// the retired `branch` key goes, comments and backup survive, the base file
/// keeps its own occurrence untouched, and a second write changes nothing.
/// The identity keys themselves are still held (their catalogue severity is
/// Warning until retirement), while the plan already names each one.
#[test]
fn migration_removes_pump_identity_keys_idempotently() {
    let (tmp, project, base_file) = init_inherited();
    append(
        &project,
        r#"
[pump]
# send identity kept until retirement
from = "dispatcher"
reply_from = "pump"
branch = "old"
[pump.board]
from = ["operator@x"]
reply_to = "operator@x"
[pump.dispatch]
from = "origin/main"
"#,
    );
    let path = config_path(&project);
    let before = fs::read_to_string(&path).unwrap();
    let base_before = fs::read_to_string(&base_file).unwrap();

    let report = migrate(Some(&project), true).unwrap();
    assert!(!report.dry_run);
    let after = fs::read_to_string(&path).unwrap();
    assert!(!after.contains("branch"), "{after}");
    for kept in [
        "# send identity kept until retirement",
        "from = \"dispatcher\"",
        "reply_from = \"pump\"",
        "from = [\"operator@x\"]",
        "reply_to = \"operator@x\"",
        "from = \"origin/main\"",
    ] {
        assert!(after.contains(kept), "{kept} missing from:\n{after}");
    }
    for named in [
        "remove [pump] from",
        "remove [pump] reply_from",
        "remove [pump.board] from",
        "remove [pump.board] reply_to",
        "remove [pump.dispatch] from",
    ] {
        assert!(
            report.plan.contains(named),
            "{named} not in:\n{}",
            report.plan
        );
    }
    let backup = report.config_backup.expect("config backup");
    assert_eq!(fs::read_to_string(&backup).unwrap(), before);
    assert_eq!(fs::read_to_string(&base_file).unwrap(), base_before);

    let again = migrate(Some(&project), true).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), after);
    assert!(!again.diff.contains("@@"), "{}", again.diff);
    assert_eq!(fs::read_to_string(&base_file).unwrap(), base_before);
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
