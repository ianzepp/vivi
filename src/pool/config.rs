//! The `[pool]` table in `.vivi/mailspace.toml`.
//!
//! Parsing never fails a mailspace on a bad pool table beyond TOML shape;
//! [`PoolConfig::validate`] runs when a `vivi pool` command starts, and also
//! reports misspelled keys, which parsing keeps in `unknown`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;

const DEFAULT_PROVIDER: &str = "fly";
const DEFAULT_REGION: &str = "iad";
const DEFAULT_SIZE: &str = "performance-8x";
const DEFAULT_MEMORY_MB: u32 = 32768;
const DEFAULT_TMPFS: &str = "16g";

/// Remote lane pool settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolConfig {
    /// Pool name; defaults to the mailspace name. Marks the pool's machines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Machine host. Only `fly` exists.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Provider application that holds the machines.
    pub app: String,
    #[serde(default = "default_region")]
    pub region: String,
    /// Source repository, `owner/name`.
    pub repo: String,
    /// Maximum pool size.
    pub slots: u32,
    /// Provider machine size (CPU class).
    #[serde(default = "default_size")]
    pub size: String,
    /// Machine memory in MB.
    #[serde(default = "default_memory")]
    pub memory: u32,
    /// Size of the memory disk mounted over the build output, e.g. `16g`.
    #[serde(default = "default_tmpfs")]
    pub tmpfs: String,
    /// Container image the machines run.
    pub image: String,
    /// Shell commands run once as root at seed time, before the checkout.
    #[serde(default)]
    pub setup: Vec<String>,
    /// Shell commands run in the checkout at seed time to warm the build output.
    #[serde(default)]
    pub seed: Vec<String>,
    /// Build output directory relative to the repo; gets the memory disk and the pack.
    pub build_dir: String,
    /// GitHub App used to mint the fetch token.
    pub github: PoolGithub,
    /// Agent harness description. Parsed and validated; nothing uses it yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<PoolHarness>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// GitHub App credentials for the fetch token.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolGithub {
    pub app_id: u64,
    /// Path to the App's PEM private key. `~` expands.
    pub private_key: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// The `[pool.harness]` table.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolHarness {
    pub name: String,
    /// Command template that runs the harness headlessly.
    pub run: String,
    /// Model slugs the harness may be given.
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

fn default_provider() -> String {
    DEFAULT_PROVIDER.to_string()
}
fn default_region() -> String {
    DEFAULT_REGION.to_string()
}
fn default_size() -> String {
    DEFAULT_SIZE.to_string()
}
fn default_memory() -> u32 {
    DEFAULT_MEMORY_MB
}
fn default_tmpfs() -> String {
    DEFAULT_TMPFS.to_string()
}

impl PoolConfig {
    /// The pool name: explicit, else the mailspace name, normalised to
    /// lowercase letters, digits, and dashes.
    #[must_use]
    pub fn pool_name(&self, mailspace_name: &str) -> String {
        let raw = self.name.as_deref().unwrap_or(mailspace_name);
        let mapped: String = raw
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        mapped.trim_matches('-').to_string()
    }

    /// The private key path with a leading `~` expanded.
    #[must_use]
    pub fn private_key_path(&self) -> PathBuf {
        match self.github.private_key.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().map_or_else(
                || PathBuf::from(&self.github.private_key),
                |home| home.join(rest),
            ),
            None => PathBuf::from(&self.github.private_key),
        }
    }

    /// Check the table before any command acts on it.
    ///
    /// # Errors
    /// Returns a configuration error naming the first bad field.
    pub fn validate(&self, mailspace_name: &str) -> Result<(), VivariumError> {
        self.validate_identity(mailspace_name)?;
        self.validate_machine()?;
        self.validate_commands()?;
        self.validate_github()?;
        if let Some(harness) = &self.harness {
            harness.validate()?;
        }
        reject_unknown("pool", &self.unknown)
    }

    fn validate_identity(&self, mailspace_name: &str) -> Result<(), VivariumError> {
        if self.provider != DEFAULT_PROVIDER {
            return Err(bad(format!(
                "provider \"{}\" is not supported (only \"fly\")",
                self.provider
            )));
        }
        if self.pool_name(mailspace_name).is_empty() {
            return Err(bad("name resolves to an empty pool name"));
        }
        for (field, value) in [("app", &self.app), ("region", &self.region)] {
            if !is_slug(value) {
                return Err(bad(format!(
                    "{field} must be lowercase letters, digits, and dashes"
                )));
            }
        }
        let mut parts = self.repo.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(name), None) if is_repo_part(owner) && is_repo_part(name) => Ok(()),
            _ => Err(bad("repo must be owner/name")),
        }
    }

    fn validate_machine(&self) -> Result<(), VivariumError> {
        if self.slots == 0 {
            return Err(bad("slots must be at least 1"));
        }
        if self.memory == 0 {
            return Err(bad("memory must be positive (MB)"));
        }
        for (field, value) in [("size", &self.size), ("image", &self.image)] {
            if value.trim().is_empty() || value.chars().any(char::is_whitespace) {
                return Err(bad(format!("{field} must be a single word")));
            }
        }
        if !is_tmpfs_size(&self.tmpfs) {
            return Err(bad("tmpfs must look like 16g, 512m, or 4096k"));
        }
        validate_build_dir(&self.build_dir)
    }

    fn validate_commands(&self) -> Result<(), VivariumError> {
        for (field, list) in [("setup", &self.setup), ("seed", &self.seed)] {
            if list.iter().any(|c| c.trim().is_empty()) {
                return Err(bad(format!("{field} holds an empty command")));
            }
        }
        Ok(())
    }

    fn validate_github(&self) -> Result<(), VivariumError> {
        if self.github.app_id == 0 {
            return Err(bad("github.app_id must be nonzero"));
        }
        if self.github.private_key.trim().is_empty() {
            return Err(bad("github.private_key must name a key file"));
        }
        reject_unknown("pool.github", &self.github.unknown)
    }
}

impl PoolHarness {
    fn validate(&self) -> Result<(), VivariumError> {
        if self.name.trim().is_empty() {
            return Err(bad("harness.name must not be empty"));
        }
        if self.run.trim().is_empty() {
            return Err(bad("harness.run must not be empty"));
        }
        if self.models.iter().any(|m| m.trim().is_empty()) {
            return Err(bad("harness.models holds an empty slug"));
        }
        let mut seen = self.models.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.models.len() {
            return Err(bad("harness.models holds a duplicate slug"));
        }
        reject_unknown("pool.harness", &self.unknown)
    }
}

fn validate_build_dir(dir: &str) -> Result<(), VivariumError> {
    let safe_chars = dir
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'));
    let escapes = dir.split('/').any(|part| part == ".." || part == ".");
    if dir.is_empty() || dir.starts_with('/') || !safe_chars || escapes || dir.contains("//") {
        return Err(bad(
            "build_dir must be a plain relative path inside the repo, e.g. target",
        ));
    }
    Ok(())
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_tmpfs_size(value: &str) -> bool {
    let Some(last) = value.chars().last() else {
        return false;
    };
    let digits = &value[..value.len() - last.len_utf8()];
    matches!(last, 'k' | 'K' | 'm' | 'M' | 'g' | 'G')
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && digits.parse::<u64>().is_ok_and(|n| n > 0)
}

fn reject_unknown(
    table: &str,
    unknown: &BTreeMap<String, toml::Value>,
) -> Result<(), VivariumError> {
    match unknown.keys().next() {
        Some(key) => Err(bad(format!("unknown field \"{key}\" in [{table}]"))),
        None => Ok(()),
    }
}

fn bad(message: impl Into<String>) -> VivariumError {
    VivariumError::Config(format!("[pool] {}", message.into()))
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
