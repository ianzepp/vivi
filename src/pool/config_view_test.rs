//! Tests for the `vivi pool list | show` views.

use super::*;
use std::fs;

/// A mailspace with two docker pools: `lanes` (harness, bands, caps) and
/// `local` (provider settings only).
fn two_pool_space() -> (tempfile::TempDir, Mailspace) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(
        dir.path().join(".vivi/mailspace.toml"),
        r#"name = "t"

[pools.local]
provider = "docker"
slots = 4
image = "ubuntu:24.04"

[pools.lanes]
provider = "docker"
slots = 2
image = "debian:12"
caps = { "m/ok" = 3 }

[pools.lanes.harness]
name = "fake"
run = "fake --model {model}"
models = ["m/ok", "m/other"]

[pools.lanes.bands]
P2 = [{ model = "m/ok", reasoning = "high" }]
P3 = [{ model = "m/other", reasoning = "low" }]
"#,
    )
    .unwrap();
    let space = Mailspace::discover(Some(dir.path())).unwrap();
    (dir, space)
}

#[test]
fn list_renders_every_pool_with_bands_and_harness_models() {
    let (_dir, space) = two_pool_space();
    let rows = list_view(Some(space.root.as_path())).unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["lanes", "local"], "config-table order");
    let lanes = &rows[0];
    assert_eq!(lanes.provider, "docker");
    assert_eq!(lanes.slots, 2);
    assert_eq!(lanes.free, None, "list never resolves free slots");
    assert_eq!(lanes.harness_models, ["m/ok", "m/other"]);
    assert_eq!(lanes.bands["P2"][0].model, "m/ok");
    assert_eq!(lanes.bands["P2"][0].reasoning, "high");
    assert_eq!(lanes.bands["P3"][0].model, "m/other");
    assert_eq!(lanes.bands["P3"][0].reasoning, "low");
    let local = &rows[1];
    assert_eq!(local.slots, 4);
    assert!(local.harness_models.is_empty() && local.bands.is_empty());
}

#[test]
fn a_row_serializes_with_the_cli_json_fields_in_order() {
    let (_dir, space) = two_pool_space();
    let rows = list_view(Some(space.root.as_path())).unwrap();
    let json = serde_json::to_string_pretty(&rows[0]).unwrap();
    assert_eq!(
        json,
        r#"{
  "name": "lanes",
  "provider": "docker",
  "slots": 2,
  "free": null,
  "bands": {
    "P2": [
      {
        "model": "m/ok",
        "reasoning": "high"
      }
    ],
    "P3": [
      {
        "model": "m/other",
        "reasoning": "low"
      }
    ]
  },
  "harness_models": [
    "m/ok",
    "m/other"
  ]
}"#
    );
}

#[test]
fn show_returns_the_row_and_the_raw_table() {
    let (_dir, space) = two_pool_space();
    let shown = show_view(Some(space.root.as_path()), "lanes", false).unwrap();
    assert_eq!(shown.row.name, "lanes");
    assert_eq!(shown.row.free, None, "not asked to resolve");
    assert_eq!(shown.pool.slots, 2, "the raw table rides along");
    assert_eq!(shown.pool.caps.get("m/ok"), Some(&3));
}

#[test]
fn show_resolves_live_free_slots_when_asked() {
    let (_dir, space) = two_pool_space();
    let shown = show_view(Some(space.root.as_path()), "lanes", true).unwrap();
    // The probe shells out to the current executable; under the test harness
    // that is not `vivi`, so the count falls back to zero.
    assert_eq!(shown.row.free, Some(0));
}

#[test]
fn show_refuses_an_unknown_pool_by_name() {
    let (_dir, space) = two_pool_space();
    let err = show_view(Some(space.root.as_path()), "nope", false).unwrap_err();
    assert!(err.to_string().contains("unknown pool 'nope'"), "{err}");
}
