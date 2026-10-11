//! The `[pool]` table in `.vivi/mailspace.toml`.
//!
//! Parsing never fails a mailspace on a bad pool table beyond TOML shape;
//! [`PoolConfig::validate`] runs when a `vivi pool` command starts, and also
//! reports misspelled keys, which parsing keeps in `unknown`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::VivariumError;

/// Machine hosts the pool can run on.
pub const PROVIDERS: [&str; 2] = ["fly", "docker"];
const DEFAULT_PROVIDER: &str = "fly";
const DEFAULT_REGION: &str = "iad";
const DEFAULT_ORGANIZATION: &str = "personal";
const DEFAULT_SIZE: &str = "performance-8x";
const DEFAULT_MEMORY_MB: u32 = 32768;
const DEFAULT_TMPFS: &str = "16g";
const DEFAULT_DOCKER_MEMORY: &str = "4g";
const DEFAULT_DOCKER_TMPFS: &str = "2g";
/// Longest Fly app name.
const MAX_APP_NAME: usize = 63;
/// Hex digits of the organization hash that end a default app name.
const APP_SUFFIX_LEN: usize = 6;

/// Remote lane pool settings. Machine-host settings live in [`PoolFly`] and
/// [`PoolDocker`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolConfig {
    /// Pool name; defaults to the mailspace name. Marks the pool's machines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Machine host: `fly` or `docker`.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Source repository, `owner/name`.
    pub repo: String,
    /// Maximum pool size.
    pub slots: u32,
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
    /// Settings that belong to the `fly` machine host.
    #[serde(default)]
    pub fly: PoolFly,
    /// Settings that belong to the `docker` machine host.
    #[serde(default)]
    pub docker: PoolDocker,
    /// GitHub App used to mint the fetch token.
    pub github: PoolGithub,
    /// Agent harness description; `dispatch` needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<PoolHarness>,
    /// Identity the remote runner commits as; `dispatch` needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committer: Option<PoolCommitter>,
    /// Source delivered with each slot instead of fetched from `repo`. Never
    /// read from TOML: only the live self-test of `vivi pool doctor --test`
    /// sets it.
    #[serde(skip)]
    pub embedded: Option<EmbeddedSource>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// A small source tree that travels with a slot and becomes a bare Git
/// remote on the slot's own disk, so jobs fetch and push locally.
#[derive(Debug, Clone, Default)]
pub struct EmbeddedSource {
    /// The `file://` address of the bare remote inside the slot.
    pub remote: String,
    /// `(guest path, contents)` of the source files.
    pub files: Vec<(String, String)>,
    /// Extra machine environment (for example boot timing).
    pub env: BTreeMap<String, String>,
}

/// The `[pool.fly]` table: the settings only the `fly` machine host has.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolFly {
    /// Provider application that holds the machines. Default: the pool name
    /// plus a short suffix derived from the organization (app names are
    /// unique across all Fly accounts). `create` makes it when it does not
    /// exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Provider organization that owns a newly created application.
    /// Default: `personal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default = "default_region")]
    pub region: String,
    /// Provider machine size (CPU class).
    #[serde(default = "default_size")]
    pub size: String,
    /// Machine memory in MB.
    #[serde(default = "default_memory")]
    pub memory: u32,
    /// Size of the memory disk mounted over the build output, e.g. `16g`.
    #[serde(default = "default_tmpfs")]
    pub tmpfs: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for PoolFly {
    fn default() -> Self {
        Self {
            app: None,
            organization: None,
            region: default_region(),
            size: default_size(),
            memory: default_memory(),
            tmpfs: default_tmpfs(),
            unknown: BTreeMap::new(),
        }
    }
}

/// The `[pool.docker]` table: the settings only the `docker` machine host has.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolDocker {
    /// Docker daemon address, e.g. `ssh://user@server`. Omit for the local
    /// daemon (or whatever `DOCKER_HOST` names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Memory limit of each slot, e.g. `4g`. The memory disk over the build
    /// output counts against it.
    #[serde(default = "default_docker_memory")]
    pub memory: String,
    /// CPU limit of each slot (fractions allowed). Default: no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<f64>,
    /// Size of the memory disk mounted over the build output, e.g. `2g`.
    #[serde(default = "default_docker_tmpfs")]
    pub tmpfs: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for PoolDocker {
    fn default() -> Self {
        Self {
            host: None,
            memory: default_docker_memory(),
            cpus: None,
            tmpfs: default_docker_tmpfs(),
            unknown: BTreeMap::new(),
        }
    }
}

impl PoolFly {
    /// The organization a new application is created in.
    #[must_use]
    pub fn organization_name(&self) -> &str {
        self.organization.as_deref().unwrap_or(DEFAULT_ORGANIZATION)
    }
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

/// The `[pool.committer]` table: the Git identity of remote commits.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolCommitter {
    pub name: String,
    pub email: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// The `[pool.harness]` table.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolHarness {
    pub name: String,
    /// Command template that runs the harness headlessly. Placeholders:
    /// `{model}`, `{brief}` (path of the task body), `{rules}` (path of the
    /// standing rules). Its standard output is the receipt.
    pub run: String,
    /// Model slugs the harness may be given.
    #[serde(default)]
    pub models: Vec<String>,
    /// Names of environment variables the harness receives, read from the
    /// machine's own environment. Names only; never values.
    #[serde(default)]
    pub env: Vec<String>,
    /// Standing rules file for the remote worker. `~` expands; a relative
    /// path is relative to the project root. Default: the embedded rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
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
fn default_docker_memory() -> String {
    DEFAULT_DOCKER_MEMORY.to_string()
}
fn default_docker_tmpfs() -> String {
    DEFAULT_DOCKER_TMPFS.to_string()
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

    /// The Fly application: explicit, else the pool name plus a suffix of
    /// the organization (see [`default_app_name`]).
    #[must_use]
    pub fn app_name(&self, mailspace_name: &str) -> String {
        self.fly.app.clone().unwrap_or_else(|| {
            default_app_name(&self.pool_name(mailspace_name), self.organization_name())
        })
    }

    /// Size of the memory disk over the build output, from the active host.
    #[must_use]
    pub fn tmpfs_size(&self) -> &str {
        if self.provider == "docker" {
            &self.docker.tmpfs
        } else {
            &self.fly.tmpfs
        }
    }

    /// The full Git address jobs and the seed fetch from and push to:
    /// GitHub for `repo = "owner/name"`, or the embedded source's own remote.
    #[must_use]
    pub fn remote_address(&self) -> String {
        match &self.embedded {
            Some(source) => source.remote.clone(),
            None => format!("https://github.com/{}.git", self.repo),
        }
    }

    /// The organization a new application is created in.
    #[must_use]
    pub fn organization_name(&self) -> &str {
        self.fly.organization_name()
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
        if let Some(committer) = &self.committer {
            committer.validate()?;
        }
        reject_moved_keys(&self.unknown)?;
        reject_unknown("pool", &self.unknown)?;
        reject_unknown("pool.fly", &self.fly.unknown)?;
        reject_unknown("pool.docker", &self.docker.unknown)
    }

    fn validate_identity(&self, mailspace_name: &str) -> Result<(), VivariumError> {
        if !PROVIDERS.contains(&self.provider.as_str()) {
            return Err(bad(format!(
                "provider \"{}\" is not supported (use \"fly\" or \"docker\")",
                self.provider
            )));
        }
        if self.pool_name(mailspace_name).is_empty() {
            return Err(bad("name resolves to an empty pool name"));
        }
        if self.provider == "fly" {
            self.validate_fly_names(mailspace_name)?;
        }
        let mut parts = self.repo.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(name), None) if is_repo_part(owner) && is_repo_part(name) => Ok(()),
            _ => Err(bad("repo must be owner/name")),
        }
    }

    fn validate_fly_names(&self, mailspace_name: &str) -> Result<(), VivariumError> {
        let app = self.app_name(mailspace_name);
        let org = self.organization_name().to_string();
        for (field, value) in [
            ("app", &app),
            ("organization", &org),
            ("region", &self.fly.region),
        ] {
            if !is_slug(value) {
                return Err(bad(format!(
                    "{field} must be lowercase letters, digits, and dashes"
                )));
            }
        }
        Ok(())
    }

    fn validate_machine(&self) -> Result<(), VivariumError> {
        if self.slots == 0 {
            return Err(bad("slots must be at least 1"));
        }
        if self.image.trim().is_empty() || self.image.chars().any(char::is_whitespace) {
            return Err(bad("image must be a single word"));
        }
        if self.provider == "docker" {
            self.docker.validate()?;
        } else {
            self.validate_fly_machine()?;
        }
        validate_build_dir(&self.build_dir)
    }

    fn validate_fly_machine(&self) -> Result<(), VivariumError> {
        if self.fly.memory == 0 {
            return Err(bad("memory must be positive (MB)"));
        }
        let size = &self.fly.size;
        if size.trim().is_empty() || size.chars().any(char::is_whitespace) {
            return Err(bad("size must be a single word"));
        }
        if !is_tmpfs_size(&self.fly.tmpfs) {
            return Err(bad("tmpfs must look like 16g, 512m, or 4096k"));
        }
        Ok(())
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

impl PoolDocker {
    fn validate(&self) -> Result<(), VivariumError> {
        if let Some(host) = &self.host
            && !is_docker_host(host)
        {
            return Err(bad(
                "docker.host must look like ssh://user@server (or tcp://, unix://)",
            ));
        }
        if !is_tmpfs_size(&self.memory) {
            return Err(bad("docker.memory must look like 4g, 512m, or 4096k"));
        }
        if !is_tmpfs_size(&self.tmpfs) {
            return Err(bad("docker.tmpfs must look like 2g, 512m, or 4096k"));
        }
        if self.cpus.is_some_and(|c| !(c.is_finite() && c > 0.0)) {
            return Err(bad("docker.cpus must be a positive number"));
        }
        Ok(())
    }
}

impl PoolCommitter {
    fn validate(&self) -> Result<(), VivariumError> {
        for (field, value) in [("name", &self.name), ("email", &self.email)] {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                return Err(bad(format!("committer.{field} must be a single line")));
            }
        }
        reject_unknown("pool.committer", &self.unknown)
    }
}

impl PoolHarness {
    /// The rules file path with `~` expanded and a relative path anchored at
    /// the project root; `None` means the embedded default.
    #[must_use]
    pub fn rules_path(&self, root: &std::path::Path) -> Option<PathBuf> {
        let raw = self.rules.as_deref()?;
        Some(match raw.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(raw), |h| h.join(rest)),
            None => root.join(raw),
        })
    }

    fn validate(&self) -> Result<(), VivariumError> {
        if self.name.trim().is_empty() {
            return Err(bad("harness.name must not be empty"));
        }
        if self.run.trim().is_empty() {
            return Err(bad("harness.run must not be empty"));
        }
        if let Some(unknown) = super::job::unknown_placeholder(&self.run) {
            return Err(bad(format!(
                "harness.run holds the unknown placeholder {{{unknown}}} (use {{model}}, {{brief}}, {{rules}})"
            )));
        }
        if self.models.iter().any(|m| !is_model_slug(m)) {
            return Err(bad(
                "harness.models holds an empty slug or one with characters outside A-Z a-z 0-9 . _ : / @ + -",
            ));
        }
        for name in &self.env {
            if !is_env_name(name) || name == "GH_TOKEN" {
                return Err(bad(format!(
                    "harness.env holds \"{name}\": use plain variable names, never GH_TOKEN"
                )));
            }
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

fn is_model_slug(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '@' | '+' | '-')
        })
}

fn is_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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

fn is_docker_host(value: &str) -> bool {
    let schemes = ["ssh://", "tcp://", "unix://"];
    schemes
        .iter()
        .any(|s| value.strip_prefix(s).is_some_and(|rest| !rest.is_empty()))
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// The Fly app a pool uses when `[pool.fly] app` is not set: the pool name
/// and a short stable suffix of the organization, so two Fly accounts that
/// both call a pool `lanes` do not fight over one name (Fly app names are
/// unique across all customers). The suffix is six hex digits of the
/// organization's SHA-256; the pool part is cut so the whole stays a valid
/// Fly app name.
#[must_use]
pub fn default_app_name(pool: &str, organization: &str) -> String {
    let digest = hex::encode(Sha256::digest(organization.as_bytes()));
    let room = MAX_APP_NAME - APP_SUFFIX_LEN - 1;
    let head: String = pool.chars().take(room).collect();
    format!(
        "{}-{}",
        head.trim_end_matches('-'),
        &digest[..APP_SUFFIX_LEN]
    )
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

/// Provider settings that used to sit directly in `[pool]`.
const MOVED_TO_FLY: [&str; 6] = ["app", "organization", "region", "size", "memory", "tmpfs"];

fn reject_moved_keys(unknown: &BTreeMap<String, toml::Value>) -> Result<(), VivariumError> {
    match MOVED_TO_FLY.iter().find(|key| unknown.contains_key(**key)) {
        Some(key) => Err(bad(format!(
            "\"{key}\" is a provider setting and now belongs in [pool.fly]; move it there"
        ))),
        None => Ok(()),
    }
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
