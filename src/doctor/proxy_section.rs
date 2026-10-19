//! The proxy section of `vivi doctor`: user-level config, instance lock,
//! service presence, and per-provider credential files.
//!
//! Credential file *contents* are never read, printed, or logged. Only paths
//! and permission bits are inspected, so a report is safe to paste.

use std::path::{Path, PathBuf};

use super::{Finding, Status};
use crate::proxy::config::{self, ProviderConfig};
use crate::proxy::paths;
use crate::proxy::status::{self, ServicePresence};

const SECTION: &str = "proxy";
const CONFIG_SKIP: &str = "no proxy config (vivi proxy is optional)";
const LOCK_FIX: &str = "vivi proxy run (or install the service)";
const SERVICE_FIX: &str = "vivi proxy install";
const SERVICE_MISSING: &str = "proxy service not installed";
const CREDENTIAL_FIX: &str = "restore the file (see vivi proxy docs)";
const CREDENTIAL_MODE: u32 = 0o600;
const ENV_SKIP: &str = "credential via environment, not checkable";
const HEALTH_DETAIL: &str = "provider health needs a live proxy";
const HEALTH_FIX: &str = "vivi proxy check";

/// Report the proxy section of `vivi doctor`.
///
/// A missing or unreadable user-level `proxy.toml` degrades to one `skipped`
/// finding: the proxy is optional, so it never fails the whole report.
#[must_use]
pub fn findings(config_path: Option<&Path>) -> Vec<Finding> {
    let Some(config) = load_config(config_path) else {
        return vec![Finding::skipped(SECTION, "config", CONFIG_SKIP)];
    };
    let mut findings = vec![
        Finding::ok(SECTION, "config", &format!("config {}", config.path.display())),
        lock_finding(&config.path),
        service_finding(),
    ];
    for (name, provider) in &config.providers {
        findings.push(credential_finding(name, provider));
    }
    findings.push(skipped_with_fix("health", HEALTH_DETAIL, HEALTH_FIX));
    findings
}

fn load_config(config_path: Option<&Path>) -> Option<config::ProxyConfig> {
    let path = paths::resolve_config_path(config_path).ok()?;
    config::load(Some(&path)).ok()
}

fn lock_finding(config_path: &Path) -> Finding {
    let lock = paths::lock_path(config_path);
    if status::lock_held(&lock) {
        Finding::ok(SECTION, "lock", "proxy running (instance lock held)")
    } else {
        Finding::warn(SECTION, "lock", "proxy not running", LOCK_FIX)
    }
}

fn service_finding() -> Finding {
    match status::service_presence() {
        ServicePresence::Installed => Finding::ok(SECTION, "service", "proxy service installed"),
        ServicePresence::Missing => Finding::warn(SECTION, "service", SERVICE_MISSING, SERVICE_FIX),
        ServicePresence::Unavailable => Finding::skipped(
            SECTION,
            "service",
            "service manager unavailable on this host",
        ),
    }
}

/// A `skipped` finding that still names the command that would cover it.
fn skipped_with_fix(name: &str, detail: &str, fix: &str) -> Finding {
    Finding {
        section: SECTION.to_string(),
        name: name.to_string(),
        status: Status::Skipped,
        detail: detail.to_string(),
        fix: Some(fix.to_string()),
    }
}

/// Check one provider's credential file: existence first, then its mode.
fn credential_finding(provider_name: &str, provider: &ProviderConfig) -> Finding {
    let name = format!("credential {provider_name}");
    let candidates = credential_candidates(provider);
    if candidates.is_empty() {
        return Finding::skipped(SECTION, &name, ENV_SKIP);
    }
    let Some(path) = first_existing(&candidates) else {
        return Finding::fail(
            SECTION,
            &name,
            &format!("credential file missing: {}", candidates[0].display()),
            CREDENTIAL_FIX,
        );
    };
    credential_mode_finding(&name, &path)
}

fn credential_candidates(provider: &ProviderConfig) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for raw in [&provider.credentials, &provider.fallback_credentials]
        .into_iter()
        .flatten()
    {
        candidates.push(paths::expand_user(Path::new(raw)));
    }
    candidates
}

fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|candidate| std::fs::metadata(candidate).is_ok())
        .cloned()
}

fn credential_mode_finding(name: &str, path: &Path) -> Finding {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Finding::fail(
            SECTION,
            name,
            &format!("credential file missing: {}", path.display()),
            CREDENTIAL_FIX,
        );
    };
    let mode = permissions_mode(&metadata) & 0o777;
    if mode == CREDENTIAL_MODE {
        Finding::ok(
            SECTION,
            name,
            &format!("credential file {} mode 0600", path.display()),
        )
    } else {
        Finding::warn(
            SECTION,
            name,
            &format!("credential permissions {mode:03o}, want 0600"),
            format!("chmod 600 {}", path.display()),
        )
    }
}

#[cfg(unix)]
fn permissions_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn permissions_mode(_metadata: &std::fs::Metadata) -> u32 {
    CREDENTIAL_MODE
}

#[cfg(test)]
#[path = "proxy_section_test.rs"]
mod tests;
