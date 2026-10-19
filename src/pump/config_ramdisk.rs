//! The `[pump.ramdisk]` table: the RAM disk a merge lane runs its checks on.
//!
//! A session with no lane RAM disk (the default) leaves the repository's own
//! build dirs in place: lanes build where they always did, and what they
//! write survives the lane. A configured lane disk gives every lane check a
//! fresh, discardable disk, so one lane's build artifacts cannot crowd or
//! corrupt another's and the disk goes away with the lane.
//!
//! Nothing here touches the repository: the cargo pairs below are prefixed
//! onto a lane's check command, and `Cargo.toml` is never edited.

use std::collections::BTreeMap;

/// Cargo settings a lane check gets by default: no incremental artifacts, no
/// debug info, symbols stripped. They cut the disk a build needs and how much
/// it writes, which is what a RAM disk is short of.
const CARGO_ENV: [(&str, &str); 5] = [
    ("CARGO_INCREMENTAL", "0"),
    ("CARGO_PROFILE_DEV_DEBUG", "0"),
    ("CARGO_PROFILE_DEV_STRIP", "symbols"),
    ("CARGO_PROFILE_TEST_DEBUG", "0"),
    ("CARGO_PROFILE_TEST_STRIP", "symbols"),
];

/// Lane RAM disk settings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PumpRamDisk {
    /// Size in GiB of the fresh RAM disk a merge lane runs its checks on.
    /// `None` leaves lanes on disk, with the repository's own build dirs in
    /// place. Set to a value in `1..=1024`.
    pub lane_gib: Option<u64>,
    /// Memory kept free for the host, in GiB, when a lane disk is admitted.
    /// Default: 6.
    pub margin_gib: u64,
    /// Environment pairs prefixed onto every lane check and proof command.
    /// They apply to that command alone; the repository's `Cargo.toml` is
    /// never touched. Defaults to the cargo pairs a RAM disk wants; a table
    /// that sets its own `env` replaces them wholesale.
    #[serde(default = "lane_env", skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl Default for PumpRamDisk {
    fn default() -> Self {
        Self {
            lane_gib: None,
            margin_gib: 6,
            env: lane_env(),
        }
    }
}

fn lane_env() -> BTreeMap<String, String> {
    CARGO_ENV
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}
