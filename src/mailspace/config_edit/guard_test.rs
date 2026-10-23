//! Guarded writes: rollback, old errors, backups, and the running-pump refusal.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use tempfile::{TempDir, tempdir};
use toml_edit::value;

use super::{guard_file, guarded};
use crate::mailspace::Mailspace;
use crate::pump::control::{self, PumpMode};

fn project() -> (TempDir, PathBuf) {
    let tmp = tempdir().unwrap();
    Mailspace::init_with_defaults(Some(tmp.path())).unwrap();
    let config = tmp.path().join(".vivi/mailspace.toml");
    (tmp, config)
}

fn append(config: &Path, text: &str) {
    let mut raw = fs::read_to_string(config).unwrap();
    raw.push_str(text);
    fs::write(config, raw).unwrap();
}

fn backups(config: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(config.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("mailspace.toml.bak."))
        .collect();
    names.sort();
    names
}

fn hold_lock(root: &Path) -> File {
    let home = root.join(".vivi/pump");
    fs::create_dir_all(&home).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    lock
}

#[test]
fn a_write_that_adds_an_error_is_restored_byte_for_byte() {
    let (tmp, config) = project();
    append(&config, "\n# keep   this\n");
    let before = fs::read(&config).unwrap();
    let err = guarded(tmp.path(), |doc| {
        doc["pump"]["tick"] = value(0);
        Ok(())
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("rolled back"), "{err}");
    assert!(err.contains("tick must be >= 1"), "{err}");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn errors_already_present_are_reported_not_blamed() {
    let (tmp, config) = project();
    append(&config, "tick = 0\n\n[pump.dispatch]\nmax_attempts = 0\n");
    let outcome = guarded(tmp.path(), |doc| {
        doc["pump"]["tick"] = value(5);
        Ok(())
    })
    .unwrap();
    assert_eq!(outcome.still.len(), 1, "{:?}", outcome.still);
    assert!(
        outcome.still[0].contains("max_attempts"),
        "{:?}",
        outcome.still
    );
    assert!(fs::read_to_string(&config).unwrap().contains("tick = 5"));
    let notes = outcome.notes();
    assert!(notes[0].starts_with("backup "), "{notes:?}");
    assert!(notes[1].starts_with("still invalid: "), "{notes:?}");
}

#[test]
fn a_write_that_adds_only_a_not_ready_item_is_kept() {
    let (tmp, config) = project();
    guarded(tmp.path(), |doc| {
        doc["pump"]["committer_name"] = value("Ann");
        doc["pump"]["committer_email"] = value("ann@example.com");
        Ok(())
    })
    .unwrap();
    guarded(tmp.path(), |doc| {
        doc["pump"]["committer_name"] = value("");
        Ok(())
    })
    .unwrap();
    assert!(
        fs::read_to_string(&config)
            .unwrap()
            .contains("committer_name = \"\"")
    );
}

#[test]
fn backups_are_pruned_to_ten_and_foreign_backups_stay() {
    let (tmp, config) = project();
    let foreign = config.with_file_name("mailspace.toml.bak.20261023T120000");
    fs::write(&foreign, "old").unwrap();
    for n in 1..=12 {
        guarded(tmp.path(), |doc| {
            doc["pump"]["tick"] = value(n);
            Ok(())
        })
        .unwrap();
    }
    let timestamped = backups(&config)
        .into_iter()
        .filter(|n| n.ends_with('Z'))
        .count();
    assert_eq!(timestamped, 10);
    assert!(foreign.exists());
}

#[test]
fn a_running_pump_refuses_the_write_and_makes_no_backup() {
    let (tmp, config) = project();
    control::set_project_mode(tmp.path(), PumpMode::Running).unwrap();
    let _lock = hold_lock(tmp.path());
    let before = fs::read(&config).unwrap();
    let err = guarded(tmp.path(), |doc| {
        doc["pump"]["tick"] = value(5);
        Ok(())
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("the pump is running"), "{err}");
    assert_eq!(fs::read(&config).unwrap(), before);
    assert!(backups(&config).is_empty());
}

#[test]
fn a_paused_pump_still_holding_its_lock_is_accepted() {
    let (tmp, config) = project();
    control::set_project_mode(tmp.path(), PumpMode::Paused).unwrap();
    let _lock = hold_lock(tmp.path());
    guarded(tmp.path(), |doc| {
        doc["pump"]["tick"] = value(5);
        Ok(())
    })
    .unwrap();
    assert!(fs::read_to_string(&config).unwrap().contains("tick = 5"));
}

#[test]
fn a_new_bad_file_is_removed_on_rollback() {
    let (tmp, _config) = project();
    let charter = tmp.path().join(".vivi/charters/merge.md");
    fs::remove_file(&charter).unwrap();
    let err = guard_file(tmp.path(), &charter, || {
        fs::write(&charter, "no envelope here\n")?;
        Ok(())
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("rolled back"), "{err}");
    assert!(!charter.exists());
}

#[test]
fn an_existing_bad_charter_is_restored_to_the_shipped_bytes() {
    let (tmp, _config) = project();
    let charter = tmp.path().join(".vivi/charters/merge.md");
    let shipped = fs::read(&charter).unwrap();
    let err = guard_file(tmp.path(), &charter, || {
        fs::write(&charter, "no envelope here\n")?;
        Ok(())
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("rolled back"), "{err}");
    assert_eq!(fs::read(&charter).unwrap(), shipped);
}

/// A config that does not load (a string where a number belongs) with an
/// error that only shows once it loads.
fn unloadable(config: &Path) {
    append(
        config,
        "tick = \"x\"\n\n[pump.dispatch]\nmax_attempts = 0\n",
    );
    assert!(Mailspace::open_root(config.parent().unwrap().parent().unwrap().into()).is_err());
}

#[test]
fn a_write_that_repairs_a_load_error_is_kept_and_reports_the_errors_it_exposed() {
    let (tmp, config) = project();
    unloadable(&config);
    let outcome = guarded(tmp.path(), |doc| {
        doc["pump"]["tick"] = value(5);
        Ok(())
    })
    .unwrap();
    assert!(fs::read_to_string(&config).unwrap().contains("tick = 5"));
    assert_eq!(outcome.still.len(), 1, "{:?}", outcome.still);
    assert!(
        outcome.still[0].contains("max_attempts"),
        "{:?}",
        outcome.still
    );
    assert!(
        outcome
            .notes()
            .iter()
            .any(|n| n.starts_with("still invalid: ") && n.contains("max_attempts")),
        "{:?}",
        outcome.notes()
    );
}

#[test]
fn a_write_that_leaves_the_load_error_is_rolled_back() {
    let (tmp, config) = project();
    unloadable(&config);
    let before = fs::read(&config).unwrap();
    let err = guarded(tmp.path(), |doc| {
        doc["pump"]["max_attempts_note"] = value(1);
        Ok(())
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("rolled back"), "{err}");
    assert!(err.contains("does not load"), "{err}");
    assert_eq!(fs::read(&config).unwrap(), before);
}
