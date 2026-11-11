//! Opt-in persistent Cargo output for one worker pool.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vivi_error::VivariumError;

use crate::pool::config::{bad, reject_unknown};

/// Only Cargo output survives a job; its checkout and temporary files do not.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolCargoTarget {
    /// Off when the table is absent.
    #[serde(default)]
    pub enabled: bool,
    /// Worker build root, on the images' filesystem. Defaults to the parent
    /// of worker.conf's apfs_dir. Targets live at root/slot-targets/pool/slot-NN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Remove an idle slot's target when its allocated size exceeds this cap.
    #[serde(default = "default_cap_gib")]
    pub cap_gib: u64,
    /// Share root/sccache across slots, with a dedicated server socket and tmp.
    #[serde(default)]
    pub sccache: bool,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

fn default_cap_gib() -> u64 {
    32
}

impl Default for PoolCargoTarget {
    fn default() -> Self {
        Self {
            enabled: false,
            root: None,
            cap_gib: default_cap_gib(),
            sccache: false,
            unknown: BTreeMap::new(),
        }
    }
}

impl PoolCargoTarget {
    pub(super) fn validate(&self, pool: &str) -> Result<(), VivariumError> {
        reject_unknown(pool, ".worker.cargo_target", &self.unknown)?;
        if !(1..=1024).contains(&self.cap_gib) {
            return Err(bad("worker.cargo_target.cap_gib must be 1..=1024 (GiB)"));
        }
        if let Some(root) = &self.root {
            let valid = root.strip_prefix('/').is_some_and(|rest| {
                !rest.is_empty()
                    && rest.split('/').all(|p| !matches!(p, "" | "." | ".."))
                    && !root.chars().any(char::is_control)
            });
            if !valid {
                return Err(bad(
                    "worker.cargo_target.root must be an absolute directory (not /, no . or .. components or control characters)",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn request(&self) -> String {
        if !self.enabled {
            return String::new();
        }
        format!(
            "\ncargo_enabled=1\ncargo_root={}\ncargo_cap_gib={}\ncargo_sccache={}",
            self.root.as_deref().unwrap_or_default(),
            self.cap_gib,
            u8::from(self.sccache)
        )
    }
}
