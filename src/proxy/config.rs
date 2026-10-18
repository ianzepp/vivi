//! `proxy.toml`: paths and commands only, never inline secrets.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::paths::{self, expand_user_str};
use crate::VivariumError;

/// Default public listener.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:18180";

/// Loaded proxy configuration.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub path: PathBuf,
    pub listen: SocketAddr,
    pub allow_remote: bool,
    pub auth_token_cmd: Option<String>,
    pub log_dir: PathBuf,
    pub providers: BTreeMap<String, ProviderConfig>,
}

/// One named backend.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: BackendKind,
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub health: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub credentials: Option<String>,
    #[serde(default)]
    pub fallback_credentials: Option<String>,
    #[serde(default)]
    pub key_cmd: Option<String>,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub token_url: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// How the front door reaches a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Upstream,
    Child,
    Native,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    allow_remote: bool,
    #[serde(default)]
    auth_token_cmd: Option<String>,
    #[serde(default)]
    log_dir: Option<String>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
}

/// Load and validate `proxy.toml`.
///
/// # Errors
///
/// Returns a configuration error when the file is missing, invalid, or
/// violates the loopback / secret-in-config rules.
pub fn load(explicit: Option<&Path>) -> Result<ProxyConfig, VivariumError> {
    let path = paths::resolve_config_path(explicit)?;
    let parsed = read_file(&path)?;
    build_config(path, parsed)
}

/// Parse TOML text as if it lived at `path` (tests and check).
///
/// # Errors
///
/// Returns a configuration error when the TOML is invalid or incomplete.
pub fn parse_at(path: PathBuf, text: &str) -> Result<ProxyConfig, VivariumError> {
    let parsed = toml::from_str(text)
        .map_err(|error| VivariumError::Config(format!("invalid {}: {error}", path.display())))?;
    build_config(path, parsed)
}

fn read_file(path: &Path) -> Result<FileConfig, VivariumError> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            VivariumError::Config(format!(
                "proxy config not found at {} (copy the example from the README)",
                path.display()
            ))
        } else {
            error.into()
        }
    })?;
    toml::from_str(&text)
        .map_err(|error| VivariumError::Config(format!("invalid {}: {error}", path.display())))
}

fn build_config(path: PathBuf, parsed: FileConfig) -> Result<ProxyConfig, VivariumError> {
    let listen = parse_listen(parsed.listen.as_deref().unwrap_or(DEFAULT_LISTEN))?;
    refuse_remote_without_auth(
        listen,
        parsed.allow_remote,
        parsed.auth_token_cmd.as_deref(),
    )?;
    if parsed.providers.is_empty() {
        return Err(VivariumError::Config(
            "proxy.toml must declare at least one [providers.<name>] table".into(),
        ));
    }
    let mut providers = BTreeMap::new();
    for (name, provider) in parsed.providers {
        validate_name(&name)?;
        providers.insert(name.clone(), validate_provider(&name, provider)?);
    }
    let log_dir = match parsed.log_dir {
        Some(raw) => paths::expand_user_str(&raw),
        None => paths::default_log_dir()?,
    };
    Ok(ProxyConfig {
        path,
        listen,
        allow_remote: parsed.allow_remote,
        auth_token_cmd: empty_to_none(parsed.auth_token_cmd),
        log_dir,
        providers,
    })
}

fn parse_listen(value: &str) -> Result<SocketAddr, VivariumError> {
    value.parse().map_err(|_| {
        VivariumError::Config(format!(
            "listen must be a socket address such as {DEFAULT_LISTEN}, got {value}"
        ))
    })
}

fn refuse_remote_without_auth(
    listen: SocketAddr,
    allow_remote: bool,
    auth_token_cmd: Option<&str>,
) -> Result<(), VivariumError> {
    if is_loopback(listen.ip()) {
        return Ok(());
    }
    let token = auth_token_cmd.map(str::trim).filter(|cmd| !cmd.is_empty());
    if allow_remote && token.is_some() {
        return Ok(());
    }
    Err(VivariumError::Config(
        "non-loopback listen requires allow_remote = true and auth_token_cmd".into(),
    ))
}

/// True for IPv4/IPv6 loopback addresses.
#[must_use]
pub fn is_loopback(ip: IpAddr) -> bool {
    ip.is_loopback()
}

fn validate_name(name: &str) -> Result<(), VivariumError> {
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if ok {
        Ok(())
    } else {
        Err(VivariumError::Config(format!(
            "provider name {name:?} must be lowercase ASCII letters, digits, or '-'"
        )))
    }
}

fn validate_provider(
    name: &str,
    mut provider: ProviderConfig,
) -> Result<ProviderConfig, VivariumError> {
    expand_provider_paths(&mut provider);
    match provider.kind {
        BackendKind::Native => validate_native(name, &provider)?,
        BackendKind::Child => validate_child(name, &provider)?,
        BackendKind::Upstream => validate_upstream(name, &provider)?,
    }
    Ok(provider)
}

fn expand_provider_paths(provider: &mut ProviderConfig) {
    provider.credentials = provider
        .credentials
        .as_deref()
        .map(expand_user_str)
        .map(|p| p.to_string_lossy().into_owned());
    provider.fallback_credentials = provider
        .fallback_credentials
        .as_deref()
        .map(expand_user_str)
        .map(|p| p.to_string_lossy().into_owned());
    provider.working_directory = provider
        .working_directory
        .as_deref()
        .map(expand_user_str)
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(command) = provider.command.as_mut() {
        for part in command.iter_mut() {
            *part = expand_user_str(part).to_string_lossy().into_owned();
        }
    }
}

fn validate_native(name: &str, provider: &ProviderConfig) -> Result<(), VivariumError> {
    let adapter = provider.adapter.as_deref().ok_or_else(|| {
        VivariumError::Config(format!("providers.{name} kind=native requires adapter"))
    })?;
    match adapter {
        "chatgpt-codex" | "kimi-coding" | "openai-passthrough" => {}
        other => {
            return Err(VivariumError::Config(format!(
                "providers.{name} unknown adapter {other:?}"
            )));
        }
    }
    if provider.key_cmd.is_none() && provider.credentials.is_none() {
        return Err(VivariumError::Config(format!(
            "providers.{name} native adapter needs credentials or key_cmd"
        )));
    }
    Ok(())
}

fn validate_child(name: &str, provider: &ProviderConfig) -> Result<(), VivariumError> {
    match provider.command.as_ref() {
        Some(command) if !command.is_empty() => {}
        _ => {
            return Err(VivariumError::Config(format!(
                "providers.{name} kind=child requires a non-empty command"
            )));
        }
    }
    require_backend(name, provider)
}

fn validate_upstream(name: &str, provider: &ProviderConfig) -> Result<(), VivariumError> {
    require_backend(name, provider)
}

fn require_backend(name: &str, provider: &ProviderConfig) -> Result<(), VivariumError> {
    match provider.backend.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => Ok(()),
        _ => Err(VivariumError::Config(format!(
            "providers.{name} requires backend = \"http://127.0.0.1:…\""
        ))),
    }
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.and_then(|text| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

impl ProviderConfig {
    /// Health path on the backend, default `/health`.
    #[must_use]
    pub fn health_path(&self) -> &str {
        self.health.as_deref().unwrap_or("/health")
    }

    /// Native adapter name, if this provider is in-process.
    #[must_use]
    pub fn native_adapter(&self) -> Option<&str> {
        if self.kind == BackendKind::Native {
            self.adapter.as_deref()
        } else {
            None
        }
    }
}
