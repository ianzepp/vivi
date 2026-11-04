//! Tests for the `harness.models` allow-list check and its guarded write.

use super::*;

/// A pool with a harness offering two models.
fn offered() -> PoolConfig {
    toml::from_str(
        r#"
provider = "local"
slots = 1

[harness]
name = "fake"
run = "fake --model {model}"
models = ["m/ok", "m/other"]
"#,
    )
    .unwrap()
}

/// A pool with no harness table at all.
fn bare() -> PoolConfig {
    toml::from_str(
        r#"
provider = "local"
slots = 1
"#,
    )
    .unwrap()
}

#[test]
fn a_model_on_the_allow_list_passes_and_returns_the_set() {
    let allowed = validate_allow("lanes", &offered(), "m/ok").unwrap();
    assert_eq!(allowed, ["m/ok", "m/other"]);
}

#[test]
fn a_model_off_the_allow_list_is_refused_naming_the_set() {
    let err = validate_allow("lanes", &offered(), "m/nope")
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "configuration error: model 'm/nope' is not in pool 'lanes' harness.models; allowed: m/ok, m/other"
    );
}

#[test]
fn a_pool_without_a_harness_allows_nothing() {
    let err = validate_allow("bare", &bare(), "m/ok")
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "configuration error: model 'm/ok' is not in pool 'bare' harness.models; allowed: (none: the pool has no harness models)"
    );
}

#[test]
fn a_harness_with_an_empty_model_list_allows_nothing() {
    let config: PoolConfig = toml::from_str(
        r#"
provider = "local"
slots = 1

[harness]
name = "fake"
run = "fake --model {model}"
models = []
"#,
    )
    .unwrap();
    let err = validate_allow("bare", &config, "m/ok")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("allowed: (none: the pool has no harness models)"),
        "got: {err}"
    );
}

/// A mailspace with one `local` pool whose harness offers two models.
fn allow_space() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join(".vivi")).unwrap();
    std::fs::write(
        root.join(".vivi/mailspace.toml"),
        r#"name = "t"

[pools.lanes]
provider = "local"
slots = 2

[pools.lanes.harness]
name = "fake"
run = "fake --model {model}"
models = ["m/ok", "m/other"]
"#,
    )
    .unwrap();
    (dir, root)
}

fn file_bytes(root: &std::path::Path) -> Vec<u8> {
    std::fs::read(root.join(".vivi/mailspace.toml")).unwrap()
}

#[test]
fn set_allow_replaces_the_list_through_the_guard() {
    let (_dir, root) = allow_space();
    set_allow(&root, "lanes", &["m/a".to_owned(), "m/b".to_owned()]).unwrap();
    let space = crate::mailspace::Mailspace::open_root(root.clone()).unwrap();
    let harness = space.config.pools["lanes"].harness.as_ref().unwrap();
    assert_eq!(harness.models, ["m/a", "m/b"]);
    let raw = String::from_utf8(file_bytes(&root)).unwrap();
    assert!(raw.contains("models = [\"m/a\", \"m/b\"]"), "got: {raw}");
}

#[test]
fn set_allow_refuses_a_bad_slug_before_writing() {
    let (_dir, root) = allow_space();
    let before = file_bytes(&root);
    let err = set_allow(&root, "lanes", &["m/a b".to_owned()])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("harness.models holds an empty slug or one with characters"),
        "got: {err}"
    );
    assert_eq!(
        file_bytes(&root),
        before,
        "a bad set must not reach the file"
    );
}

#[test]
fn set_allow_refuses_a_duplicate_slug_before_writing() {
    let (_dir, root) = allow_space();
    let before = file_bytes(&root);
    let err = set_allow(&root, "lanes", &["m/a".to_owned(), "m/a".to_owned()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("holds a duplicate slug"), "got: {err}");
    assert_eq!(
        file_bytes(&root),
        before,
        "a bad set must not reach the file"
    );
}

#[test]
fn set_allow_refuses_an_unknown_pool_before_writing() {
    let (_dir, root) = allow_space();
    let before = file_bytes(&root);
    let err = set_allow(&root, "nope", &["m/a".to_owned()])
        .unwrap_err()
        .to_string();
    assert_eq!(err, "configuration error: unknown pool 'nope'");
    assert_eq!(
        file_bytes(&root),
        before,
        "an unknown pool must not reach the file"
    );
}
