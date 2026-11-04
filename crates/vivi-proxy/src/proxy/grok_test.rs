use chrono::{TimeZone, Utc};
use serde_json::json;

use super::*;
use crate::proxy::config::{BackendKind, ProviderConfig};

fn provider() -> ProviderConfig {
    ProviderConfig {
        kind: BackendKind::Native,
        adapter: Some("xai-grok".into()),
        command: None,
        backend: None,
        health: None,
        models: Vec::new(),
        credentials: None,
        fallback_credentials: None,
        key_cmd: None,
        upstream: None,
        token_url: None,
        client_id: None,
        client_version: None,
        session_header: None,
        working_directory: None,
        env: std::collections::BTreeMap::default(),
    }
}

fn entry(key: &str, expires_at: &str) -> Value {
    entry_with(key, "r1", expires_at)
}

fn entry_with(key: &str, refresh: &str, expires_at: &str) -> Value {
    json!({"key": key, "refresh_token": refresh, "expires_at": expires_at, "email": "a@b.c"})
}

#[test]
fn selects_the_entry_for_the_token_issuer() {
    let root = json!({
        "https://auth.x.ai::c1": entry("k", "2030-01-01T00:00:00Z"),
        "https://other.example::c2": entry("o", "2030-01-01T00:00:00Z"),
    });
    assert_eq!(
        entry_name(&root, &provider()).unwrap(),
        "https://auth.x.ai::c1"
    );
}

#[test]
fn ambiguous_entries_name_the_count_and_never_a_token() {
    let root = json!({
        "https://auth.x.ai::c1": entry("SECRET-ONE", "2030-01-01T00:00:00Z"),
        "https://auth.x.ai::c2": entry("SECRET-TWO", "2030-01-01T00:00:00Z"),
    });
    let text = entry_name(&root, &provider()).unwrap_err().to_string();
    assert!(text.contains("2 entries"), "{text}");
    assert!(!text.contains("SECRET"), "{text}");
    let mut pinned = provider();
    pinned.client_id = Some("c2".into());
    assert_eq!(entry_name(&root, &pinned).unwrap(), "https://auth.x.ai::c2");
}

#[test]
fn missing_issuer_entry_is_an_error() {
    let root = json!({"https://other.example::c": entry("k", "2030-01-01T00:00:00Z")});
    assert!(entry_name(&root, &provider()).is_err());
    assert!(entry_name(&json!([]), &provider()).is_err());
}

#[test]
fn expiry_skew_decides_refresh() {
    let now = Utc
        .with_ymd_and_hms(2026, 10, 22, 12, 0, 0)
        .unwrap()
        .timestamp();
    assert!(!needs_refresh(&entry("k", "2026-10-22T13:00:00Z"), now));
    assert!(needs_refresh(&entry("k", "2026-10-22T12:00:20Z"), now));
    assert!(needs_refresh(&entry("k", "2026-10-22T11:00:00Z"), now));
    assert!(needs_refresh(&json!({"refresh_token": "r"}), now));
    assert!(!needs_refresh(&json!({"key": "static"}), now));
}

#[test]
fn rejected_token_is_not_usable() {
    let now = Utc
        .with_ymd_and_hms(2026, 10, 22, 12, 0, 0)
        .unwrap()
        .timestamp();
    let fresh = entry("k", "2026-10-22T13:00:00Z");
    assert_eq!(usable(&fresh, None, now).as_deref(), Some("k"));
    assert_eq!(usable(&fresh, Some("other"), now).as_deref(), Some("k"));
    assert_eq!(usable(&fresh, Some("k"), now), None);
}

#[test]
fn apply_refresh_updates_only_token_fields() {
    let now = Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap();
    let mut item = entry("old", "2026-10-22T11:00:00Z");
    item["user_id"] = json!("u1");
    let payload = json!({"access_token": "new", "refresh_token": "r2", "expires_in": 3600});
    apply_refresh(&mut item, &payload, now).unwrap();
    assert_eq!(item["key"], "new");
    assert_eq!(item["refresh_token"], "r2");
    assert_eq!(item["expires_at"], "2026-10-22T13:00:00.000000Z");
    assert_eq!(item["email"], "a@b.c");
    assert_eq!(item["user_id"], "u1");
}

#[test]
fn apply_refresh_keeps_the_refresh_token_when_not_rotated() {
    let now = Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap();
    let mut item = entry("old", "2026-10-22T11:00:00Z");
    apply_refresh(
        &mut item,
        &json!({"access_token": "new", "expires_in": 60}),
        now,
    )
    .unwrap();
    assert_eq!(item["refresh_token"], "r1");
}

#[test]
fn apply_refresh_without_access_token_fails_without_echo() {
    let now = Utc::now();
    let mut item = entry("old", "2026-10-22T11:00:00Z");
    let error = apply_refresh(&mut item, &json!({"refresh_token": "LEAK"}), now).unwrap_err();
    assert!(!error.to_string().contains("LEAK"));
    assert_eq!(item["key"], "old");
}

#[test]
fn upstream_and_token_defaults() {
    assert_eq!(upstream(&provider()), "https://api.x.ai/v1");
    assert_eq!(token_url(&provider()), "https://auth.x.ai/oauth2/token");
    assert_eq!(issuer(&provider()).unwrap(), "https://auth.x.ai");
}

fn write_file(dir: &tempfile::TempDir, root: &Value) -> std::path::PathBuf {
    let path = dir.path().join("auth.json");
    std::fs::write(&path, root.to_string()).unwrap();
    path
}

fn configured(path: &std::path::Path) -> ProviderConfig {
    let mut item = provider();
    item.credentials = Some(path.to_string_lossy().into_owned());
    item.token_url = Some("http://127.0.0.1:1/oauth2/token".into());
    item
}

fn issuer_key(item: &ProviderConfig) -> String {
    format!("{}::cid", issuer(item).unwrap())
}

#[tokio::test]
async fn a_newer_token_on_disk_beats_the_rejected_one_without_a_wire_call() {
    let dir = tempfile::tempdir().unwrap();
    let mut item = configured(&dir.path().join("auth.json"));
    let name = issuer_key(&item);
    let path = write_file(
        &dir,
        &json!({ name: entry_with("disk-new", "r9", "2099-01-01T00:00:00Z") }),
    );
    item.credentials = Some(path.to_string_lossy().into_owned());
    let client = reqwest::Client::new();
    let token = bearer(&client, &item, Some("rejected-old")).await.unwrap();
    assert_eq!(token, "disk-new");
}

#[tokio::test]
async fn an_unreachable_token_endpoint_is_a_failure_not_a_dead_grant() {
    let dir = tempfile::tempdir().unwrap();
    let mut item = configured(&dir.path().join("auth.json"));
    let name = issuer_key(&item);
    let path = write_file(
        &dir,
        &json!({ name: entry_with("old", "r1", "2020-01-01T00:00:00Z") }),
    );
    item.credentials = Some(path.to_string_lossy().into_owned());
    let client = reqwest::Client::new();
    let error = bearer(&client, &item, None).await.unwrap_err();
    assert_eq!(error, Unavailable::Failed);
    assert!(!dead::is_dead(&path));
}

#[test]
fn a_refused_refresh_defers_to_a_token_rotated_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let mut item = configured(&dir.path().join("auth.json"));
    let name = issuer_key(&item);
    let path = write_file(
        &dir,
        &json!({ name: entry_with("rotated", "r9", "2099-01-01T00:00:00Z") }),
    );
    item.credentials = Some(path.to_string_lossy().into_owned());
    assert_eq!(rotated_elsewhere(&path, &item, "r1").unwrap(), "rotated");
    assert_eq!(
        rotated_elsewhere(&path, &item, "r9"),
        Err(Unavailable::Dead)
    );
    assert!(dead::is_dead(&path));
}

#[test]
fn state_and_health_labels_never_carry_a_token() {
    let dir = tempfile::tempdir().unwrap();
    let mut item = configured(&dir.path().join("auth.json"));
    assert_eq!(credential_state(&item), "missing");
    assert_eq!(health_status(&item), "missing");
    let name = issuer_key(&item);
    let path = write_file(
        &dir,
        &json!({ name: entry_with("SECRET", "r1", "2099-01-01T00:00:00Z") }),
    );
    item.credentials = Some(path.to_string_lossy().into_owned());
    assert_eq!(credential_state(&item), "2099-01-01T00:00:00+00:00");
    assert_eq!(health_status(&item), "ok");
    dead::mark_dead(&path);
    assert_eq!(credential_state(&item), "dead");
    assert_eq!(health_status(&item), "credential_dead");
    item.key_cmd = Some("printf x".into());
    assert_eq!(credential_state(&item), "key_cmd");
    assert_eq!(health_status(&item), "ok");
}
