//! The `[pools.<name>.ramdisk]` table: whether a job on a `local` pool runs
//! on a RAM disk this machine makes for it, and what that disk holds.
//!
//! Only `local` pools may enable it: the disk is this machine's memory, so
//! there is nothing to make on a remote host, and a container or machine host
//! already has its own memory disk over the build directory. A job's disk is
//! made fresh for that job and torn down with it, so nothing here survives a
//! job.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{bad, reject_unknown};
use crate::error::VivariumError;

/// Size of a job's RAM disk when the table sets none: enough for a hand
/// lane's build, not for a gate pool's full test build.
const DEFAULT_SIZE_GIB: u64 = 8;
/// Memory kept out of the admission sum for the system and the host's own
/// work when a job's RAM disk is planned.
const DEFAULT_MARGIN_GIB: u64 = 6;
/// Cargo settings a job on a RAM disk gets by default: no incremental
/// artifacts, no debug info, symbols stripped. They cut the disk a build needs
/// and its write volume, which is what a RAM disk is short of.
const CARGO_ENV: [(&str, &str); 5] = [
    ("CARGO_INCREMENTAL", "0"),
    ("CARGO_PROFILE_DEV_DEBUG", "0"),
    ("CARGO_PROFILE_TEST_DEBUG", "0"),
    ("CARGO_PROFILE_DEV_STRIP", "symbols"),
    ("CARGO_PROFILE_TEST_STRIP", "symbols"),
];

/// RAM disk settings for one pool.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolRamDisk {
    /// Give each job its own RAM disk. Off by default, and only a `local`
    /// pool may turn it on.
    #[serde(default)]
    pub enabled: bool,
    /// Disk size in GiB, `1..=1024`. Default: 8, enough for a hand lane and
    /// not for a gate pool's full test build (which sets 32).
    #[serde(default = "default_size_gib")]
    pub size_gib: u64,
    /// Memory kept out of the admission sum for this machine, in GiB,
    /// `0..=64`. Default: 6.
    #[serde(default = "default_margin_gib")]
    pub margin_gib: u64,
    /// Environment given to a job on the RAM disk. Defaults to the cargo
    /// settings a RAM disk wants ([`CARGO_ENV`]); a table that sets its own
    /// `env` replaces them wholesale.
    #[serde(default = "default_env")]
    pub env: BTreeMap<String, String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for PoolRamDisk {
    fn default() -> Self {
        Self {
            enabled: false,
            size_gib: default_size_gib(),
            margin_gib: default_margin_gib(),
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

impl PoolRamDisk {
    /// The environment a job on the RAM disk gets, sorted by name; empty when
    /// the table is off.
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
    /// this machine's memory to make a disk from, and the sizes must be sane.
    /// `provider` is the pool's own host.
    ///
    /// # Errors
    /// Returns a configuration error naming the first bad field.
    pub fn validate(&self, pool: &str, provider: &str) -> Result<(), VivariumError> {
        reject_unknown(pool, ".ramdisk", &self.unknown)?;
        if self.enabled && provider != "local" {
            return Err(bad(format!(
                "a ramdisk is a disk in this machine's memory, so [pools.{pool}.ramdisk] applies to the local provider only; this pool's provider is \"{provider}\""
            )));
        }
        if !(1..=1024).contains(&self.size_gib) {
            return Err(bad(format!(
                "ramdisk.size_gib must be 1..=1024 (GiB), got {}",
                self.size_gib
            )));
        }
        if self.margin_gib > 64 {
            return Err(bad(format!(
                "ramdisk.margin_gib must be 0..=64 (GiB), got {}",
                self.margin_gib
            )));
        }
        Ok(())
    }
}
