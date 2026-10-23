//! Flow settings for the `[pump.flow]` table.
//!
//! Unset means no cap on live runs.

/// Flow settings (the `[pump.flow]` table).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FlowConfig {
    /// Most runs that may be starting, live or restarting at once; unset
    /// means no cap.
    pub max_live_runs: Option<u32>,
}

impl FlowConfig {
    /// Whether every key still has its default (nothing was written).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
#[path = "flow_test.rs"]
mod tests;
