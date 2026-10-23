//! Rung settings for the `[pump.rungs]` table and the per-target override
//! `[pump.targets.<id>.rungs]`.
//!
//! A rung is a named proof level: `task`, `need`, `main` and `release`. Each
//! holds the shell command that proves that level; `timeout` bounds one run
//! and `pools` names the pools a no-model rung job may run on.

/// The four rung names, in climbing order.
pub const RUNG_NAMES: [&str; 4] = ["task", "need", "main", "release"];

/// Default seconds one rung run may take.
pub const DEFAULT_RUNG_TIMEOUT: u64 = 1800;

/// Rung settings (the `[pump.rungs]` table).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RungsConfig {
    /// Command proving a task (one unit) before it is delivered.
    pub task: Option<String>,
    /// Command proving a whole need before it is accepted.
    pub need: Option<String>,
    /// Command proving the main branch after promotion.
    pub main: Option<String>,
    /// Command proving a release.
    pub release: Option<String>,
    /// Seconds one rung run may take.
    pub timeout: u64,
    /// Pools a no-model rung job may run on; empty means any pool.
    pub pools: Vec<String>,
}

impl Default for RungsConfig {
    fn default() -> Self {
        Self {
            task: None,
            need: None,
            main: None,
            release: None,
            timeout: DEFAULT_RUNG_TIMEOUT,
            pools: Vec::new(),
        }
    }
}

impl RungsConfig {
    /// Whether every key still has its default (nothing was written).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The command named `name`, or none for an unset or unknown rung.
    #[must_use]
    pub fn command(&self, name: &str) -> Option<&str> {
        match name {
            "task" => self.task.as_deref(),
            "need" => self.need.as_deref(),
            "main" => self.main.as_deref(),
            "release" => self.release.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "rungs_test.rs"]
mod tests;
