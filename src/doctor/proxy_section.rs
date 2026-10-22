//! The user-level proxy section: config, listener/provider health, and native
//! credential-file presence and permissions.

use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use super::Finding;
use crate::proxy::{
    config::{self, BackendKind, ProviderConfig, ProxyConfig},
    paths,
};

const SECTION: &str = "proxy";
const CONFIG_FIX: &str = "repair the user proxy config at the reported path";

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Report whether the user-level proxy is configured, listening, and serving
/// healthy providers. It reads credential metadata only; it never runs
/// credential commands or reads credential contents.
#[must_use]
pub fn findings(config_path: Option<&Path>) -> Vec<Finding> {
    let path = match paths::resolve_config_path(config_path) {
        Ok(path) => path,
        Err(_) => {
            return vec![Finding::skipped(
                SECTION,
                "config",
                "proxy config location unavailable",
            )];
        }
    };
    if !path.is_file() {
        return vec![Finding::skipped(SECTION, "config", "proxy not configured")];
    }
    let config = match config::load(config_path) {
        Ok(config) => config,
        Err(_) => {
            return vec![Finding::fail(
                SECTION,
                "config",
                "proxy config unreadable or invalid",
                CONFIG_FIX,
            )];
        }
    };
    let mut out = vec![Finding::ok(SECTION, "config", "proxy config parsed")];
    let listening = listener_is_up(&config);
    out.push(if listening {
        Finding::ok(SECTION, "service", "proxy listener reachable")
    } else {
        Finding::warn(
            SECTION,
            "service",
            "proxy listener is not reachable",
            "vivi proxy service install (or vivi proxy run)",
        )
    });
    out.extend(provider_findings(&config, listening));
    out.extend(credential_findings(&config));
    out
}

fn listener_is_up(config: &ProxyConfig) -> bool {
    TcpStream::connect_timeout(&config.listen, Duration::from_millis(300)).is_ok()
}

fn provider_findings(config: &ProxyConfig, listening: bool) -> Vec<Finding> {
    config
        .providers
        .iter()
        .map(|(name, _)| {
            if !listening {
                return Finding::skipped(
                    SECTION,
                    "provider",
                    &format!("{name}: proxy listener unavailable"),
                );
            }
            match provider_status(config, name) {
                ProviderProbe::Status("ok") => {
                    Finding::ok(SECTION, "provider", &format!("{name}: healthy"))
                }
                ProviderProbe::Status(status) => Finding::warn(
                    SECTION,
                    "provider",
                    &format!("{name}: {status}"),
                    &format!("vivi proxy providers; repair {name} health or credentials"),
                ),
                ProviderProbe::AuthRequired => Finding::skipped(
                    SECTION,
                    "provider",
                    &format!("{name}: health check requires proxy authentication"),
                ),
                ProviderProbe::Unavailable => Finding::warn(
                    SECTION,
                    "provider",
                    &format!("{name}: health check unavailable"),
                    &format!("vivi proxy providers; check {name}"),
                ),
            }
        })
        .collect()
}

enum ProviderProbe {
    Status(&'static str),
    AuthRequired,
    Unavailable,
}

fn provider_status(config: &ProxyConfig, name: &str) -> ProviderProbe {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build();
    let Ok(client) = client else {
        return ProviderProbe::Unavailable;
    };
    let url = format!("http://{}/{name}/health", config.listen);
    let response = match client.get(url).send() {
        Ok(response) => response,
        Err(_) => return ProviderProbe::Unavailable,
    };
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return ProviderProbe::AuthRequired;
    }
    let ok_http = response.status().is_success();
    let body = response.text().unwrap_or_default();
    ProviderProbe::Status(crate::proxy::health::intern_status(&body, ok_http))
}

fn credential_findings(config: &ProxyConfig) -> Vec<Finding> {
    config
        .providers
        .iter()
        .filter(|(_, provider)| provider.kind == BackendKind::Native)
        .map(|(name, provider)| credential_finding(name, provider))
        .collect()
}

fn credential_finding(name: &str, provider: &ProviderConfig) -> Finding {
    if provider.key_cmd.is_some() {
        return Finding::skipped(
            SECTION,
            "credential",
            &format!("{name}: credentials supplied by key_cmd"),
        );
    }
    let path = native_credential_path(provider);
    let Some(path) = path else {
        return Finding::warn(
            SECTION,
            "credential",
            &format!("{name}: credential file missing"),
            "restore the configured proxy credential file",
        );
    };
    let shown = path.display().to_string();
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Finding::warn(
            SECTION,
            "credential",
            &format!("{name}: credential file missing at {shown}"),
            "restore the configured proxy credential file",
        );
    };
    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o600 {
            return Finding::warn(
                SECTION,
                "credential",
                &format!("{name}: {shown} permissions are {mode:04o}, expected 0600"),
                "chmod 600 <credential-file>",
            );
        }
        Finding::ok(
            SECTION,
            "credential",
            &format!("{name}: credential file present with mode 0600 at {shown}"),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Finding::skipped(
            SECTION,
            "credential",
            &format!("{name}: platform does not expose Unix file modes for {shown}"),
        )
    }
}

fn native_credential_path(provider: &ProviderConfig) -> Option<std::path::PathBuf> {
    if let Some(path) = provider.credentials.as_deref() {
        return Some(paths::expand_user_str(path));
    }
    if let Some(path) = provider.fallback_credentials.as_deref() {
        if std::path::Path::new(path).is_file() {
            return Some(paths::expand_user_str(path));
        }
    }
    match provider.adapter.as_deref() {
        Some("chatgpt-codex") => crate::proxy::codex_auth::path_if_present(provider),
        Some("xai-grok") => crate::proxy::grok::credential_path_if_present(provider),
        Some("kimi-coding" | "openai-passthrough") => {
            crate::proxy::kimi::credential_path_if_present(provider)
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "proxy_section_test.rs"]
mod tests;
