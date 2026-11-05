use super::*;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("creds")).unwrap();
    private(&dir.join("creds"), 0o700);
    dir
}

/// Set a mode: the server refuses secrets other users can read.
fn private(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn write_secret(home: &Path, name: &str, text: &str) {
    let path = home.join("creds").join(name);
    std::fs::write(&path, text).unwrap();
    private(&path, 0o600);
}

fn with_config(tag: &str, text: &str) -> PathBuf {
    let dir = scratch(tag);
    std::fs::write(dir.join("config.toml"), text).unwrap();
    dir
}

#[test]
fn an_empty_home_is_every_default_and_no_secrets() {
    let home = scratch("defaults");
    let config = ServerConfig::load_from(&home).unwrap();
    assert_eq!(config.bind, IpAddr::from([127, 0, 0, 1]));
    assert_eq!(config.port, 8080);
    assert_eq!(config.data_dir, home.join("data"));
    assert_eq!(config.host.projects_root, Some(home.join("projects")));
    assert_eq!(config.host.lanes_root, Some(home.join("lanes")));
    assert_eq!(
        config.host.git_url_prefix,
        Some(format!("file://{}/", home.join("lanes").display()))
    );
    assert!(config.store_encryption_key.is_none());
    assert!(config.moderation_token.is_none());
    assert!(config.host_operator_token.is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn bind_names_the_interface_to_serve_on() {
    let home = with_config("bind", "bind = \"100.96.126.80\"\nport = 18290\n");
    let config = ServerConfig::load_from(&home).unwrap();
    assert_eq!(config.bind, IpAddr::from([100, 96, 126, 80]));
    assert_eq!(config.port, 18290);
    let bad = with_config("bind-bad", "bind = \"tailnet\"\n");
    let err = ServerConfig::load_from(&bad).unwrap_err();
    assert!(err.contains("config.toml"), "{err}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&bad);
}

#[test]
fn relative_paths_are_under_the_home_and_absolute_paths_are_kept() {
    let home = with_config(
        "paths",
        "data_dir = \"store\"\nlanes_root = \"/srv/lanes\"\n",
    );
    let config = ServerConfig::load_from(&home).unwrap();
    assert_eq!(config.data_dir, home.join("store"));
    assert_eq!(config.host.lanes_root, Some(PathBuf::from("/srv/lanes")));
    assert_eq!(
        config.host.git_url_prefix.as_deref(),
        Some("file:///srv/lanes/")
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_unknown_key_is_refused() {
    let home = with_config("unknown", "prot = 8080\n");
    let err = ServerConfig::load_from(&home).unwrap_err();
    assert!(err.contains("prot"), "{err}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_credential_file_is_read_and_trimmed() {
    let home = scratch("creds");
    write_secret(&home, "MODERATION_TOKEN", "from-file\n");
    let config = ServerConfig::load_from(&home).unwrap();
    assert_eq!(config.moderation_token.as_deref(), Some("from-file"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_unreadable_credential_is_an_error_not_a_fallback() {
    let home = scratch("unreadable");
    std::fs::create_dir(home.join("creds/STORE_ENCRYPTION_KEY")).unwrap();
    private(&home.join("creds/STORE_ENCRYPTION_KEY"), 0o700);
    let err = ServerConfig::load_from(&home).unwrap_err();
    assert!(err.starts_with("cannot read credential"), "{err}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_blank_operator_token_counts_as_unset() {
    let home = scratch("blank");
    write_secret(&home, "HOST_OPERATOR_TOKEN", "\n");
    let config = ServerConfig::load_from(&home).unwrap();
    assert!(config.host_operator_token.is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn hosted_identity_keeps_its_old_rules() {
    let home = with_config("identity", "hosted_default_identity = \" ops.1 \"\n");
    let config = ServerConfig::load_from(&home).unwrap();
    assert_eq!(config.hosted_default_identity.as_deref(), Some("ops.1"));
    let bad = with_config("identity-bad", "hosted_default_identity = \"a b\"\n");
    assert!(ServerConfig::load_from(&bad).is_err());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&bad);
}

#[test]
fn send_rate_zero_is_unlimited() {
    let home = with_config("rate", "send_rate_per_minute = 0\n");
    let config = ServerConfig::load_from(&home).unwrap();
    assert_eq!(config.send_rate_per_minute, None);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn secrets_other_users_can_read_are_refused() {
    let home = scratch("open-file");
    write_secret(&home, "MODERATION_TOKEN", "t\n");
    private(&home.join("creds/MODERATION_TOKEN"), 0o644);
    let err = ServerConfig::load_from(&home).unwrap_err();
    assert!(err.contains("chmod"), "{err}");
    let _ = std::fs::remove_dir_all(&home);

    let home = scratch("open-dir");
    private(&home.join("creds"), 0o755);
    let err = ServerConfig::load_from(&home).unwrap_err();
    assert!(err.contains("chmod"), "{err}");
    let _ = std::fs::remove_dir_all(&home);
}

/// The default log filter must let the server's own lines through. It named
/// `vivi::server` after the crates were split (the server's lines come from
/// `vivi_server::...`, the pumps' from `vivi_core::pump::...`), and a 23 host
/// logged nothing at all: no home, no projects, no listening line.
#[test]
fn the_default_log_filter_keeps_the_server_and_pump_lines() {
    use std::sync::{Arc, Mutex};
    let home = scratch("log-filter");
    let config = ServerConfig::load_from(&home).unwrap();
    let seen: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&config.log))
        .with_writer(move || SinkWriter(sink.clone()))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("a server line");
        tracing::info!(target: "vivi_core::pump::service", "a pump line");
        tracing::debug!("a debug line");
    });
    let text = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
    assert!(text.contains("a server line"), "{text:?}");
    assert!(text.contains("a pump line"), "{text:?}");
    assert!(!text.contains("a debug line"), "{text:?}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A writer into a shared buffer, for capturing log lines.
struct SinkWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SinkWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
