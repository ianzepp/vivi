//! The fetch depth of a pool: the `depth` setting, its default, and its range.

use super::config::PoolConfig;
use crate::error::VivariumError;

/// The Git fetch depth a pool uses when it sets no `depth`: recent history
/// enough for a job to build and test, never the full history.
pub const DEFAULT_FETCH_DEPTH: u32 = 50;

/// The largest fetch depth a pool may set.
pub const MAX_FETCH_DEPTH: u32 = 100_000;

impl PoolConfig {
    /// The Git fetch depth of this pool's slots and jobs: the `depth` setting
    /// when one is set, [`DEFAULT_FETCH_DEPTH`] otherwise.
    #[must_use]
    pub fn fetch_depth(&self) -> u32 {
        self.depth.unwrap_or(DEFAULT_FETCH_DEPTH)
    }

    /// Check the pool's `depth` setting: a positive integer within range.
    /// Zero would fetch nothing at all, so it is refused by name.
    ///
    /// # Errors
    /// Returns a configuration error naming the setting when `depth` is
    /// outside 1..=[`MAX_FETCH_DEPTH`].
    pub fn check_depth(&self, pool: &str) -> Result<(), VivariumError> {
        match self.depth {
            None => Ok(()),
            Some(depth) if (1..=MAX_FETCH_DEPTH).contains(&depth) => Ok(()),
            Some(depth) => Err(VivariumError::Config(format!(
                "[pools.{pool}] depth must be between 1 and {MAX_FETCH_DEPTH}, got {depth}"
            ))),
        }
    }
}

#[cfg(test)]
#[path = "config_depth_test.rs"]
mod tests;
