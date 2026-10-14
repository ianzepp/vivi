//! Pump configuration (the `[pump]` table of `mailspace.toml`).

/// Pump settings (the `[pump]` table in `mailspace.toml`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PumpConfig {}
