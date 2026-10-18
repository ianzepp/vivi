use serde_json::json;

use super::codex_auth;
use super::credentials;

#[test]
fn atomic_write_is_0600_and_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    credentials::write_json_atomic(&path, &json!({"access_token": "t"})).unwrap();
    let value = credentials::read_json(&path).unwrap();
    assert_eq!(value["access_token"], "t");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn apply_refresh_writes_nested_tokens() {
    let mut store = json!({"tokens": {"access_token": "old", "refresh_token": "r1"}});
    codex_auth::apply_refresh_for_test(
        &mut store,
        &json!({"access_token": "new", "refresh_token": "r2"}),
    )
    .unwrap();
    assert_eq!(store["tokens"]["access_token"], "new");
    assert_eq!(store["tokens"]["refresh_token"], "r2");
}

#[test]
fn needs_refresh_without_token() {
    assert!(codex_auth::needs_refresh(&json!({}), 0));
}
