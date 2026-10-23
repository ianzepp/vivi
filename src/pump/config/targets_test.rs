//! Tests for `pump::config::targets`.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::{TempDir, tempdir};

use super::{TargetSpec, add, list, remove, split_pair};
use crate::mailspace::Mailspace;

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

/// A project with a pool and workspace rung pools, so a rung can be set.
fn ready() -> (TempDir, PathBuf) {
    let (tmp, config) = project();
    append(
        &config,
        "\n[pools.p]\nprovider = \"local\"\nslots = 1\n\n[pump.rungs]\npools = [\"p\"]\n",
    );
    (tmp, config)
}

fn spec(id: &str) -> TargetSpec {
    TargetSpec {
        id: id.to_owned(),
        path: String::from("repo"),
        ..TargetSpec::default()
    }
}

#[test]
fn add_writes_path_main_rungs_and_typed_merge() {
    let (tmp, config) = ready();
    fs::create_dir_all(tmp.path().join("repo")).unwrap();
    let mut s = spec("a");
    s.main = Some(String::from("trunk"));
    s.rungs = vec![(String::from("task"), String::from("cargo test"))];
    s.merge = vec![
        (String::from("remote_limit"), String::from("600")),
        (String::from("format"), String::from("cargo fmt")),
    ];
    add(tmp.path(), &s).unwrap();
    let raw = fs::read_to_string(&config).unwrap();
    assert!(raw.contains("[pump.targets.a]"), "{raw}");
    assert!(raw.contains("main = \"trunk\""), "{raw}");
    assert!(raw.contains("task = \"cargo test\""), "{raw}");
    assert!(raw.contains("remote_limit = 600"), "{raw}");
    assert!(raw.contains("format = \"cargo fmt\""), "{raw}");
    let rows = list(tmp.path()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "a");
    assert_eq!(rows[0].main, "trunk");
    assert_eq!(rows[0].rungs, vec!["task".to_string()]);
}

#[test]
fn add_of_an_existing_id_is_refused_and_leaves_the_file() {
    let (tmp, config) = ready();
    fs::create_dir_all(tmp.path().join("repo")).unwrap();
    add(tmp.path(), &spec("a")).unwrap();
    let before = fs::read(&config).unwrap();
    let err = add(tmp.path(), &spec("a")).unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn add_with_a_bad_rung_or_merge_key_writes_nothing() {
    let (tmp, config) = ready();
    let before = fs::read(&config).unwrap();
    let mut s = spec("a");
    s.rungs = vec![(String::from("bogus"), String::from("x"))];
    assert!(add(tmp.path(), &s).is_err());
    let mut s = spec("a");
    s.rungs = vec![(String::from("task"), String::from("  "))];
    assert!(add(tmp.path(), &s).is_err());
    let mut s = spec("a");
    s.merge = vec![(String::from("generated"), String::from("x"))];
    let err = add(tmp.path(), &s).unwrap_err().to_string();
    assert!(err.contains("mailspace.toml"), "{err}");
    let mut s = spec("a");
    s.merge = vec![(String::from("nope"), String::from("1"))];
    let err = add(tmp.path(), &s).unwrap_err().to_string();
    assert!(err.contains("unknown merge setting `nope`"), "{err}");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn add_with_a_path_outside_the_project_is_rolled_back() {
    let (tmp, config) = ready();
    let before = fs::read(&config).unwrap();
    let mut s = spec("a");
    s.path = String::from("../x");
    let err = add(tmp.path(), &s).unwrap_err().to_string();
    assert!(err.contains("rolled back"), "{err}");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn add_with_a_rung_but_no_rung_pools_is_rolled_back() {
    let (tmp, config) = project();
    fs::create_dir_all(tmp.path().join("repo")).unwrap();
    let before = fs::read(&config).unwrap();
    let mut s = spec("a");
    s.rungs = vec![(String::from("task"), String::from("cargo test"))];
    let err = add(tmp.path(), &s).unwrap_err().to_string();
    assert!(err.contains("rolled back"), "{err}");
    assert_eq!(fs::read(&config).unwrap(), before);
}

#[test]
fn remove_deletes_the_table_and_the_empty_targets_table() {
    let (tmp, config) = ready();
    fs::create_dir_all(tmp.path().join("repo")).unwrap();
    add(tmp.path(), &spec("a")).unwrap();
    remove(tmp.path(), "a").unwrap();
    let raw = fs::read_to_string(&config).unwrap();
    assert!(!raw.contains("pump.targets"), "{raw}");
    assert!(list(tmp.path()).unwrap().is_empty());
    let err = remove(tmp.path(), "a").unwrap_err().to_string();
    assert!(err.contains("unknown pump target `a`"), "{err}");
}

#[test]
fn split_pair_splits_on_the_first_equals() {
    assert_eq!(
        split_pair("task=a=b", "rung").unwrap(),
        ("task".to_string(), "a=b".to_string())
    );
    let err = split_pair("task", "rung").unwrap_err().to_string();
    assert!(err.contains("--rung wants NAME=VALUE"), "{err}");
    assert!(split_pair("=x", "rung").is_err());
}
