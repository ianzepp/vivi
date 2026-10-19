use std::collections::BTreeMap;

/// Lane RAM disk settings from `[pump.ramdisk]`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PumpRamDisk {
    /// Per-lane disk size; `None` disables lane disks.
    pub lane_gib: Option<u64>,
    /// GiB reserved for the host.
    pub margin_gib: u64,
    /// Environment for lane checks and proofs only.
    #[serde(default = "default_env", skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl Default for PumpRamDisk {
    fn default() -> Self {
        Self { lane_gib: None, margin_gib: 6, env: default_env() }
    }
}

fn default_env() -> BTreeMap<String, String> {
    [
        ("CARGO_INCREMENTAL", "0"),
        ("CARGO_PROFILE_DEV_DEBUG", "0"),
        ("CARGO_PROFILE_TEST_DEBUG", "0"),
        ("CARGO_PROFILE_DEV_STRIP", "symbols"),
        ("CARGO_PROFILE_TEST_STRIP", "symbols"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect()
}

impl PumpRamDisk {
    pub(crate) fn validate(&self, errors: &mut Vec<String>) {
        if let Some(gib) = self.lane_gib.filter(|gib| !(1..=1024).contains(gib)) {
            errors.push(format!(
                "[pump.ramdisk] lane_gib must be between 1 and 1024, got {gib}"
            ));
        }
    }
}

#[cfg(test)]
#[path = "config_ramdisk_test.rs"]
mod tests;
