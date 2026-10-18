use super::config::{self, BackendKind, DEFAULT_LISTEN};

fn parse(text: &str) -> config::ProxyConfig {
    config::parse_at(std::path::PathBuf::from("/tmp/proxy.toml"), text).expect("parse")
}

#[test]
fn default_listen_is_loopback() {
    let config = parse(
        r#"
        [providers.llama]
        kind = "upstream"
        backend = "http://127.0.0.1:18173"
        "#,
    );
    assert_eq!(config.listen.to_string(), DEFAULT_LISTEN);
    assert_eq!(config.providers["llama"].kind, BackendKind::Upstream);
}

#[test]
fn refuses_remote_without_token() {
    let error = config::parse_at(
        std::path::PathBuf::from("/tmp/proxy.toml"),
        r#"
        listen = "0.0.0.0:18180"
        [providers.llama]
        kind = "upstream"
        backend = "http://127.0.0.1:18173"
        "#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("allow_remote"));
}

#[test]
fn allows_remote_with_token_and_flag() {
    let config = parse(
        r#"
        listen = "0.0.0.0:18180"
        allow_remote = true
        auth_token_cmd = "printf secret"
        [providers.llama]
        kind = "upstream"
        backend = "http://127.0.0.1:18173"
        "#,
    );
    assert!(config.allow_remote);
    assert_eq!(config.auth_token_cmd.as_deref(), Some("printf secret"));
}

#[test]
fn native_requires_adapter_and_credentials() {
    let error = config::parse_at(
        std::path::PathBuf::from("/tmp/proxy.toml"),
        r#"
        [providers.kimi]
        kind = "native"
        "#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("adapter"));
}

#[test]
fn expands_tilde_in_credential_path() {
    let config = parse(
        r#"
        [providers.kimi]
        kind = "native"
        adapter = "kimi-coding"
        credentials = "~/creds.json"
        "#,
    );
    let path = config.providers["kimi"].credentials.as_deref().unwrap();
    assert!(!path.starts_with('~'));
    assert!(path.ends_with("creds.json"));
}
