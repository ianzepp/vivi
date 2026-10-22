use super::config::{BackendKind, ProviderConfig};
use super::kimi;

fn provider() -> ProviderConfig {
    ProviderConfig {
        kind: BackendKind::Native,
        adapter: Some("kimi-coding".into()),
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
        client_version: Some("1.15.0".into()),
        session_header: None,
        working_directory: None,
        env: std::collections::BTreeMap::default(),
    }
}

#[test]
fn msh_headers_use_kimi_cli_names() {
    let headers = kimi::msh_headers("1.15.0", &provider());
    let names: Vec<&str> = headers.iter().map(|(name, _)| *name).collect();
    assert!(names.contains(&"x-msh-platform"));
    assert!(names.contains(&"x-msh-version"));
    assert!(names.contains(&"x-msh-device-id"));
    let version = headers
        .iter()
        .find(|(name, _)| *name == "x-msh-version")
        .map(|(_, value)| value.as_str());
    assert_eq!(version, Some("1.15.0"));
}

#[test]
fn passthrough_sends_only_the_bearer() {
    let mut passthrough = provider();
    passthrough.adapter = Some("openai-passthrough".into());
    let headers = kimi::identity_headers(&passthrough, "t");
    assert_eq!(headers.len(), 1);
    assert!(headers.contains_key("authorization"));
}

#[test]
fn kimi_coding_sends_its_client_identity() {
    let mut coding = provider();
    coding.adapter = Some("kimi-coding".into());
    let headers = kimi::identity_headers(&coding, "t");
    assert!(headers.contains_key("user-agent"));
    assert!(headers.contains_key("x-msh-platform"));
}

fn passthrough_with_key_cmd() -> ProviderConfig {
    let mut item = provider();
    item.adapter = Some("openai-passthrough".into());
    item.key_cmd = Some("echo k".into());
    item
}

fn kimi_with_key_cmd() -> ProviderConfig {
    let mut item = provider();
    item.key_cmd = Some("echo k".into());
    item
}

/// A credential file already marked dead, as after one refused refresh.
fn dead_credential(dir: &tempfile::TempDir) -> String {
    let path = dir.path().join("kimi-code.json");
    std::fs::write(&path, r#"{"access_token":"a","refresh_token":"r"}"#).expect("write");
    crate::proxy::dead::mark_dead(&path);
    path.to_string_lossy().into_owned()
}

#[test]
fn dead_kimi_credential_does_not_refuse_a_passthrough_provider() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut item = passthrough_with_key_cmd();
    // Even a dead file named on the passthrough provider itself is not its death.
    item.credentials = Some(dead_credential(&dir));
    assert!(kimi::refuse_if_dead(&item, "req").is_none());
    assert_eq!(kimi::health_status(&item), "ok");
    assert_eq!(kimi::credential_state(&item), "key_cmd");
}

#[test]
fn dead_kimi_credential_still_refuses_a_kimi_provider() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut item = provider();
    item.credentials = Some(dead_credential(&dir));
    let refused = kimi::refuse_if_dead(&item, "req").expect("refused");
    assert_eq!(refused.status(), axum::http::StatusCode::BAD_GATEWAY);
    assert_eq!(kimi::health_status(&item), "credential_dead");
    assert_eq!(kimi::credential_state(&item), "dead");
}

#[test]
fn dead_credential_file_does_not_refuse_a_passthrough_file_provider() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut item = provider();
    item.adapter = Some("openai-passthrough".into());
    item.credentials = Some(dead_credential(&dir));
    assert!(kimi::refuse_if_dead(&item, "req").is_none());
    assert_eq!(kimi::health_status(&item), "ok");
}

#[test]
fn kimi_key_cmd_provider_is_never_dead() {
    let dir = tempfile::tempdir().expect("tmp");
    let mut item = kimi_with_key_cmd();
    item.credentials = Some(dead_credential(&dir));
    assert!(kimi::refuse_if_dead(&item, "req").is_none());
}

#[test]
fn home_credential_fallbacks_belong_to_the_kimi_adapter_only() {
    let has_home = |item: &ProviderConfig| {
        kimi::credential_candidates(item)
            .iter()
            .any(|path| path.to_string_lossy().contains("kimi-code.json"))
    };
    assert!(has_home(&provider()));
    assert!(!has_home(&passthrough_with_key_cmd()));
}

#[tokio::test]
async fn health_label_names_the_adapter_not_kimi_for_passthrough() {
    async fn label(item: &ProviderConfig) -> String {
        let (response, _) = kimi::handle(
            &reqwest::Client::new(),
            item,
            &crate::proxy::route::Action::Health,
            axum::http::Method::GET,
            None,
            axum::http::HeaderMap::new(),
            bytes::Bytes::new(),
            "req",
        )
        .await
        .expect("health");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        value["provider"].as_str().unwrap_or_default().to_string()
    }
    assert_eq!(
        label(&passthrough_with_key_cmd()).await,
        "openai-passthrough"
    );
    assert_eq!(label(&kimi_with_key_cmd()).await, "kimi");
}
