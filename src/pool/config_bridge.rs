//! The `[pools.<name>.bridge]` table: whether a dispatch on the pool mints
//! an MCP bridge grant for the job.
//!
//! One boolean, off by default; an absent table is the same as off. The
//! server and the project id the grant is for come from the dispatching
//! mailspace's own `[sync]` table, so there are no settings here to mistype
//! and no credential ever lives in this table.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::reject_unknown;
use crate::error::VivariumError;

/// MCP bridge settings for one pool.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PoolBridge {
    /// A dispatch on this pool mints one project API key for the job,
    /// labelled `pool-<pool>-<job>`, through the dispatching mailspace's
    /// member identity, and delivers it only inside the job archive; the
    /// runner gives it to the agent as `VIVI_MCP_API_KEY`.
    #[serde(default)]
    pub enabled: bool,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl PoolBridge {
    /// Check the table before any command acts on it.
    ///
    /// # Errors
    /// Returns a configuration error naming the first unknown field.
    pub fn validate(&self, pool: &str) -> Result<(), VivariumError> {
        reject_unknown(pool, ".bridge", &self.unknown)
    }
}
