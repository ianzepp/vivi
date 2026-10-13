//! The `[pools.<name>.bridge]` table: whether a dispatch on the pool mints
//! an MCP bridge grant for the job.
//!
//! One boolean, off by default; an absent table is the same as off. The
//! server and the project id the grant is for come from the dispatching
//! mailspace's own `[sync]` table, so there are no settings here to mistype
//! and no credential ever lives in this table.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::{bad, reject_unknown};
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

    /// Check the harness template against the bridge: the `{mcp_config}`
    /// placeholder names the `mcp.json` file that travels only with a grant,
    /// so a template that uses it on a pool whose bridge is off would run a
    /// harness pointing at a file that is not there. A bridge pool whose
    /// harness does not use the placeholder is fine.
    ///
    /// # Errors
    /// Returns a configuration error naming the placeholder and the pool's
    /// bridge table.
    pub fn validate_harness(
        &self,
        pool: &str,
        harness_run: Option<&str>,
    ) -> Result<(), VivariumError> {
        let Some(run) = harness_run else {
            return Ok(());
        };
        if run.contains("{mcp_config}") && !self.enabled {
            return Err(bad(format!(
                "harness.run uses the placeholder {{mcp_config}} but [pools.{pool}.bridge] is \
                 not enabled: the mcp.json file it names is carried only when the bridge is on"
            )));
        }
        Ok(())
    }
}
