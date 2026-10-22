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

#[test]
fn xai_grok_requires_credentials_or_key_cmd() {
    let error = config::parse_at(
        std::path::PathBuf::from("/tmp/proxy.toml"),
        r#"
        [providers.grok]
        kind = "native"
        adapter = "xai-grok"
        "#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("credentials or key_cmd"));
    let config = parse(
        r#"
        [providers.grok]
        kind = "native"
        adapter = "xai-grok"
        credentials = "~/.grok/auth.json"
        "#,
    );
    assert_eq!(config.providers["grok"].native_adapter(), Some("xai-grok"));
    let config = parse(
        r#"
        [providers.grok]
        kind = "native"
        adapter = "xai-grok"
        key_cmd = "printf x"
        "#,
    );
    assert!(config.providers["grok"].key_cmd.is_some());
}

#[test]
fn documented_opencode_go_block_parses_and_prints_its_key() {
    let config = parse(
        r##"
        [providers.opencode-go]
        kind = "native"
        adapter = "openai-passthrough"
        upstream = "https://opencode.ai/zen/go/v1"
        key_cmd = "python3 -c 'import json,os;print(json.load(open(os.path.expanduser(\"~/.local/share/opencode/auth.json\")))[\"opencode-go\"][\"key\"])'"
        "##,
    );
    let command = config.providers["opencode-go"].key_cmd.as_deref().unwrap();
    assert!(command.contains(r#"expanduser("~/.local/share/opencode/auth.json")"#));
}

#[test]
fn passthrough_session_header_parses() {
    let config = parse(
        r#"
        [providers.opencode-go]
        kind = "native"
        adapter = "openai-passthrough"
        upstream = "https://opencode.ai/zen/go/v1"
        key_cmd = "echo k"
        session_header = "x-opencode-session"
        "#,
    );
    assert_eq!(
        config.providers["opencode-go"].session_header.as_deref(),
        Some("x-opencode-session")
    );
}

#[test]
fn session_header_rejects_bad_names_and_other_adapters() {
    let bad = |adapter: &str, header: &str| {
        config::parse_at(
            std::path::PathBuf::from("/tmp/proxy.toml"),
            &format!(
                "[providers.p]\nkind = \"native\"\nadapter = \"{adapter}\"\nkey_cmd = \"echo k\"\nsession_header = \"{header}\"\n"
            ),
        )
        .unwrap_err()
        .to_string()
    };
    assert!(bad("openai-passthrough", "authorization").contains("session_header"));
    assert!(bad("openai-passthrough", "Bad Name").contains("session_header"));
    assert!(bad("kimi-coding", "x-opencode-session").contains("openai-passthrough"));
}
