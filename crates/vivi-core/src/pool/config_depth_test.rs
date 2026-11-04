use super::*;

use crate::pool::slot::PoolId;

/// A pool config with an optional top-level `depth` setting. The setting
/// belongs to `[pools.<name>]`, so it is written before the `[local]` table.
fn config(depth: Option<u32>) -> PoolConfig {
    let setting = depth.map_or_else(String::new, |d| format!("depth = {d}\n"));
    toml::from_str(&format!(
        "provider = \"local\"\nslots = 4\n{setting}[local]\nroot = \"~/lanes\"\n"
    ))
    .unwrap()
}

fn id() -> PoolId {
    PoolId::new("space", "", "main")
}

#[test]
fn no_depth_setting_fetches_the_default_fifty() {
    let config = config(None);
    assert_eq!(config.depth, None);
    assert_eq!(config.fetch_depth(), DEFAULT_FETCH_DEPTH);
    assert_eq!(config.fetch_depth(), 50);
    config.validate(&id()).unwrap();
}

#[test]
fn a_depth_setting_overrides_the_default() {
    let config = config(Some(7));
    assert_eq!(config.depth, Some(7));
    assert_eq!(config.fetch_depth(), 7);
    config.validate(&id()).unwrap();
}

#[test]
fn depth_zero_is_refused_and_names_the_setting() {
    let err = config(Some(0)).validate(&id()).unwrap_err().to_string();
    assert!(err.contains("[pools.main] depth"), "got: {err}");
}

#[test]
fn a_depth_over_the_range_is_refused_too() {
    let err = config(Some(MAX_FETCH_DEPTH + 1))
        .validate(&id())
        .unwrap_err()
        .to_string();
    assert!(err.contains("[pools.main] depth"), "got: {err}");
}
