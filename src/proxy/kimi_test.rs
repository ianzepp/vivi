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
