use super::*;

use crate::pool::slot::PoolId;

/// A pool config with an optional top-level `restore` setting. The setting
/// belongs to `[pools.<name>]`, so it is written before the `[local]` table.
fn config(restore: &[&str]) -> PoolConfig {
    let setting = if restore.is_empty() {
        String::new()
    } else {
        let list = restore
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("restore = [{list}]\n")
    };
    toml::from_str(&format!(
        "provider = \"local\"\nslots = 4\n{setting}[local]\nroot = \"~/lanes\"\n"
    ))
    .unwrap()
}

fn id() -> PoolId {
    PoolId::new("space", "main")
}

#[test]
fn no_restore_setting_leaves_the_list_empty_and_validates() {
    let config = config(&[]);
    assert_eq!(config.restore, Vec::<String>::new());
    config.validate(&id()).unwrap();
}

#[test]
fn a_list_of_relative_paths_validates() {
    let config = config(&["runtime/llvm/Cargo.lock", "a b/c.txt"]);
    assert_eq!(config.restore, ["runtime/llvm/Cargo.lock", "a b/c.txt"]);
    config.validate(&id()).unwrap();
}

#[test]
fn an_empty_path_is_refused_and_names_the_setting() {
    let err = config(&[""]).validate(&id()).unwrap_err().to_string();
    assert!(err.contains("[pools.main] restore"), "got: {err}");
}

#[test]
fn an_absolute_path_is_refused() {
    let err = config(&["/etc/passwd"])
        .validate(&id())
        .unwrap_err()
        .to_string();
    assert!(err.contains("[pools.main] restore"), "got: {err}");
    assert!(err.contains("relative"), "got: {err}");
}

#[test]
fn a_dot_dot_component_is_refused() {
    for path in ["../escape", "a/../b", "a/.."] {
        let err = config(&[path]).validate(&id()).unwrap_err().to_string();
        assert!(err.contains("[pools.main] restore"), "got: {err}");
        assert!(err.contains(".."), "got: {err}");
    }
    let ok = config(&["a..b", "x/..y/z"]);
    ok.validate(&id()).unwrap();
}

#[test]
fn a_control_character_is_refused() {
    let err = config(&["a\nb"]).validate(&id()).unwrap_err().to_string();
    assert!(err.contains("[pools.main] restore"), "got: {err}");
    let err = config(&["a\tb"]).validate(&id()).unwrap_err().to_string();
    assert!(err.contains("[pools.main] restore"), "got: {err}");
}

#[test]
fn more_than_a_hundred_paths_are_refused() {
    let mut config = config(&[]);
    config.restore = (0..=MAX_RESTORE_PATHS)
        .map(|i| format!("f{i}.txt"))
        .collect();
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(err.contains("[pools.main] restore"), "got: {err}");
    assert!(err.contains("100"), "got: {err}");
    config.restore.truncate(MAX_RESTORE_PATHS);
    config.validate(&id()).unwrap();
}
