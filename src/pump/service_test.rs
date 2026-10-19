//! Tests for [`crate::pump::service`].

use std::collections::BTreeSet;
use std::path::PathBuf;

use tempfile::tempdir;

use super::load_config_at;

/// A config file listing projects parses into ordered paths.
#[test]
fn load_config_at_parses_projects() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("pump-service.toml");
    std::fs::write(&path, "projects = [\"/tmp/b\", \"/tmp/a\"]\n").unwrap();
    let config = load_config_at(&path).unwrap();
    let expected: BTreeSet<PathBuf> = [PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")]
        .into_iter()
        .collect();
    assert_eq!(config.projects, expected);
}

/// A missing config file is an empty registry, not an error.
#[test]
fn load_config_at_missing_file_is_empty() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("absent.toml");
    let config = load_config_at(&path).unwrap();
    assert!(config.projects.is_empty());
}

/// Unknown keys are rejected and the error names the file.
#[test]
fn load_config_at_rejects_unknown_fields() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("pump-service.toml");
    std::fs::write(&path, "wat = 1\n").unwrap();
    let error = load_config_at(&path).unwrap_err().to_string();
    assert!(error.contains(&path.display().to_string()), "{error}");
}
