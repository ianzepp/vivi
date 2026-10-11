//! The `[pools.<name>.runpod]` table: the settings only the `runpod` machine
//! host has. A slot is one rented pod that lives for one job.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::config::{bad, reject_unknown};
use crate::error::VivariumError;

const DEFAULT_GPU_COUNT: u32 = 1;
const MAX_GPU_COUNT: u32 = 8;
const DEFAULT_DISK_GB: u32 = 40;
const MAX_DISK_GB: u32 = 4096;
const DEFAULT_CLOUD: &str = "secure";

/// The `[pools.<name>.runpod]` table.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolRunpod {
    /// File that holds the RunPod API key. Required. `~/` means the home
    /// directory. Vivi never reads it: the path goes to `runpodctl`, which
    /// reads the key. The file must not be readable by group or others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<String>,
    /// GPU type ids (as `runpodctl gpu list` spells them), best first: the
    /// first one with stock wins and the rest are fallbacks. Required.
    #[serde(default)]
    pub gpu_types: Vec<String>,
    /// GPUs per pod.
    #[serde(default = "default_gpu_count")]
    pub gpu_count: u32,
    /// `secure` or `community`.
    #[serde(default = "default_cloud")]
    pub cloud: String,
    /// Container disk of each pod in GB (a pod keeps nothing else).
    #[serde(default = "default_disk")]
    pub container_disk_gb: u32,
    /// Lowest CUDA version the host's driver must support, e.g. `12.8`. An
    /// image alone does not guarantee a driver floor: hosts differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cuda_version: Option<String>,
    /// Data centre ids to rent in (empty: any).
    #[serde(default)]
    pub data_centers: Vec<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

fn default_gpu_count() -> u32 {
    DEFAULT_GPU_COUNT
}
fn default_cloud() -> String {
    DEFAULT_CLOUD.to_string()
}
fn default_disk() -> u32 {
    DEFAULT_DISK_GB
}

impl Default for PoolRunpod {
    fn default() -> Self {
        Self {
            api_key_file: None,
            gpu_types: Vec::new(),
            gpu_count: DEFAULT_GPU_COUNT,
            cloud: default_cloud(),
            container_disk_gb: DEFAULT_DISK_GB,
            min_cuda_version: None,
            data_centers: Vec::new(),
            unknown: BTreeMap::new(),
        }
    }
}

impl PoolRunpod {
    /// The key file path with a leading `~/` expanded.
    #[must_use]
    pub fn key_path(&self) -> PathBuf {
        let raw = self.api_key_file.as_deref().unwrap_or_default();
        match raw.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(raw), |h| h.join(rest)),
            None => PathBuf::from(raw),
        }
    }

    /// The cloud type as `runpodctl` spells it.
    #[must_use]
    pub fn cloud_type(&self) -> &'static str {
        if self.cloud == "community" {
            "COMMUNITY"
        } else {
            "SECURE"
        }
    }

    pub(super) fn validate(&self, pool: &str) -> Result<(), VivariumError> {
        let Some(key) = self.api_key_file.as_deref() else {
            return Err(bad(
                "runpod.api_key_file is required (the file that holds the RunPod API key; Vivi hands its path to runpodctl and never reads it)",
            ));
        };
        check_key_path(key)?;
        self.validate_hardware()?;
        reject_unknown(pool, ".runpod", &self.unknown)
    }

    fn validate_hardware(&self) -> Result<(), VivariumError> {
        if self.gpu_types.is_empty() || self.gpu_types.iter().any(|g| !is_gpu_id(g)) {
            return Err(bad(
                "runpod.gpu_types must list at least one GPU type id, as `runpodctl gpu list` spells it (letters, digits, spaces and - . _ only)",
            ));
        }
        let mut seen = self.gpu_types.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.gpu_types.len() {
            return Err(bad("runpod.gpu_types holds a duplicate"));
        }
        if !(1..=MAX_GPU_COUNT).contains(&self.gpu_count) {
            return Err(bad(format!(
                "runpod.gpu_count must be between 1 and {MAX_GPU_COUNT}"
            )));
        }
        if !matches!(self.cloud.as_str(), "secure" | "community") {
            return Err(bad("runpod.cloud must be \"secure\" or \"community\""));
        }
        if !(1..=MAX_DISK_GB).contains(&self.container_disk_gb) {
            return Err(bad(format!(
                "runpod.container_disk_gb must be between 1 and {MAX_DISK_GB}"
            )));
        }
        if self
            .min_cuda_version
            .as_deref()
            .is_some_and(|v| !is_cuda_version(v))
        {
            return Err(bad("runpod.min_cuda_version must look like 12.8"));
        }
        if self.data_centers.iter().any(|d| !is_data_center(d)) {
            return Err(bad(
                "runpod.data_centers holds an id that is not letters, digits and dashes (for example EU-RO-1)",
            ));
        }
        Ok(())
    }
}

/// A key file path: `~/<path>` or absolute, no control character.
fn check_key_path(path: &str) -> Result<(), VivariumError> {
    let rest = path.strip_prefix("~/").or_else(|| path.strip_prefix('/'));
    if rest.is_none_or(str::is_empty) || path.chars().any(char::is_control) {
        return Err(bad(
            "runpod.api_key_file must be a file path: /abs/path or ~/path",
        ));
    }
    Ok(())
}

fn is_gpu_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 100
        && value.trim() == value
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '.' | '_'))
}

fn is_cuda_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let digits =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    digits(parts.next()) && digits(parts.next()) && parts.next().is_none()
}

fn is_data_center(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
#[path = "runpod_config_test.rs"]
mod tests;
