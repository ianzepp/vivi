//! The `[pools.<name>.workspace]` table: whether a job on a `local` pool runs
//! on a fresh disk this machine makes for it, what backs that disk, and what
//! the disk holds.
//!
//! Only `local` pools may enable it: the disk is this machine's memory or an
//! APFS volume on this machine's storage, so there is nothing to make on a
//! remote host, and a container or machine host already has its own memory
//! disk over the build directory. A job's disk is made fresh for that job and
//! torn down with it, so nothing here survives a job.
//!
//! Two backings, chosen by `kind`:
//!
//! - `ram` (the default): a RAM disk of `size_gib`, admitted against free
//!   memory.
//! - `apfs`: an APFS volume with a quota of `size_gib` in the container that
//!   holds the pool, admitted against the container's free space and, for
//!   the compilers, free memory. Quota only, never a reserve: a reserve claims
//!   its space up front and cannot be shrunk in place.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{bad, reject_unknown};
use crate::error::VivariumError;

/// Size of a job's disk when the table sets none: enough for a hand lane's
/// build, not for a gate pool's full test build.
const DEFAULT_SIZE_GIB: u64 = 8;
/// Memory kept out of the admission sum for the system and the host's own
/// work when a job's disk is planned.
const DEFAULT_MARGIN_GIB: u64 = 6;
/// Cargo settings a job on a workspace disk gets by default: no incremental
/// artifacts, no debug info, symbols stripped. They cut the disk a build needs
/// and its write volume.
const CARGO_ENV: [(&str, &str); 5] = [
    ("CARGO_INCREMENTAL", "0"),
    ("CARGO_PROFILE_DEV_DEBUG", "0"),
    ("CARGO_PROFILE_TEST_DEBUG", "0"),
    ("CARGO_PROFILE_DEV_STRIP", "symbols"),
    ("CARGO_PROFILE_TEST_STRIP", "symbols"),
];

/// What backs a job's workspace disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    /// A RAM disk made from this machine's memory.
    #[default]
    Ram,
    /// An APFS volume with a quota, in an APFS container on this machine.
    Apfs,
}

/// Workspace disk settings for one pool.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolWorkspace {
    /// Give each job its own disk. Off by default, and only a `local` pool may
    /// turn it on.
    #[serde(default)]
    pub enabled: bool,
    /// What backs the disk: `ram` (default) or `apfs`.
    #[serde(default)]
    pub kind: WorkspaceKind,
    /// Disk size in GiB, `1..=1024`: the RAM disk's size, or the APFS volume's
    /// quota. Default: 8, enough for a hand lane and not for a gate pool's full
    /// test build (which sets 32).
    #[serde(default = "default_size_gib")]
    pub size_gib: u64,
    /// Memory kept out of the admission sum for this machine, in GiB,
    /// `0..=64`. Default: 6. For `apfs` it is also the free space kept in the
    /// container.
    #[serde(default = "default_margin_gib")]
    pub margin_gib: u64,
    /// The APFS container that holds `apfs` volumes, as `diskN`. Unset: the
    /// container that holds the pool's root. Only `apfs` may set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Environment given to a job on the disk. Defaults to the cargo settings
    /// a workspace disk wants ([`CARGO_ENV`]); a table that sets its own `env`
    /// replaces them wholesale.
    #[serde(default = "default_env")]
    pub env: BTreeMap<String, String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for PoolWorkspace {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: WorkspaceKind::default(),
            size_gib: default_size_gib(),
            margin_gib: default_margin_gib(),
            container: None,
            env: default_env(),
            unknown: BTreeMap::new(),
        }
    }
}

fn default_size_gib() -> u64 {
    DEFAULT_SIZE_GIB
}

fn default_margin_gib() -> u64 {
    DEFAULT_MARGIN_GIB
}

fn default_env() -> BTreeMap<String, String> {
    CARGO_ENV
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

impl PoolWorkspace {
    /// The environment a job on the disk gets, sorted by name; empty when the
    /// table is off.
    #[must_use]
    pub fn job_env(&self) -> Vec<(String, String)> {
        if !self.enabled {
            return Vec::new();
        }
        self.env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    }

    /// Check the table before any command acts on it: only a `local` pool has
    /// this machine's memory or storage to make a disk from, and the sizes
    /// must be sane. `provider` is the pool's own host.
    ///
    /// # Errors
    /// Returns a configuration error naming the first bad field.
    pub fn validate(&self, pool: &str, provider: &str) -> Result<(), VivariumError> {
        reject_unknown(pool, ".workspace", &self.unknown)?;
        if self.enabled && provider != "local" {
            return Err(bad(format!(
                "a workspace disk is made on this machine, so [pools.{pool}.workspace] applies to the local provider only; this pool's provider is \"{provider}\""
            )));
        }
        if !(1..=1024).contains(&self.size_gib) {
            return Err(bad(format!(
                "workspace.size_gib must be 1..=1024 (GiB), got {}",
                self.size_gib
            )));
        }
        if self.margin_gib > 64 {
            return Err(bad(format!(
                "workspace.margin_gib must be 0..=64 (GiB), got {}",
                self.margin_gib
            )));
        }
        check_container(self.kind, self.container.as_deref(), "workspace.container")?;
        check_env(&self.env, "workspace.env")
    }
}

/// `container` names an APFS container (`diskN`) and is set only for `apfs`.
///
/// # Errors
/// Returns a configuration error naming `field`.
pub fn check_container(
    kind: WorkspaceKind,
    container: Option<&str>,
    field: &str,
) -> Result<(), VivariumError> {
    let Some(container) = container else {
        return Ok(());
    };
    if kind != WorkspaceKind::Apfs {
        return Err(bad(format!(
            "{field} names an APFS container, so it applies only with kind = \"apfs\""
        )));
    }
    let digits = container.strip_prefix("disk").unwrap_or_default();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad(format!(
            "{field} must be an APFS container like \"disk3\", got {container:?}"
        )));
    }
    Ok(())
}

/// Every `env` key is an environment name (never `GH_TOKEN`) and no value
/// holds a NUL.
fn check_env(env: &BTreeMap<String, String>, field: &str) -> Result<(), VivariumError> {
    if let Some(name) = env
        .keys()
        .find(|name| !is_env_name(name) || name.as_str() == "GH_TOKEN")
    {
        return Err(bad(format!(
            "{field} key {name:?} is not an allowed environment name"
        )));
    }
    if env.values().any(|value| value.contains('\0')) {
        return Err(bad(format!("{field} values cannot contain NUL")));
    }
    Ok(())
}

fn is_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
