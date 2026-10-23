//! Rung settings for the `[pump.rungs]` table and the per-target override
//! `[pump.targets.<id>.rungs]`.
//!
//! A rung is a named proof level: `task`, `need`, `main` and `release`. Each
//! holds the shell command that proves that level; `timeout` bounds one run
//! and `pools` names the pools a no-model rung job may run on.

use std::path::Path;

use crate::error::VivariumError;
use crate::mailspace::config_edit;
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR, Mailspace};

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

impl RungsConfig {
    /// Render the rung settings of the project at `project_root` for
    /// `vivi pump rungs show`. With `target`, each command is the effective one
    /// (the target's own rung, else the workspace-level one).
    pub fn show(project_root: &Path, target: Option<&str>) -> Result<String, VivariumError> {
        let space = Mailspace::discover(Some(project_root))?;
        let cfg = space.config.pump.unwrap_or_default();
        if let Some(id) = target
            && !cfg.targets.contains_key(id)
        {
            return Err(VivariumError::Config(format!("unknown pump target `{id}`")));
        }
        let mut out = String::new();
        for name in RUNG_NAMES {
            let command = match target {
                Some(id) => cfg.rung(id, name),
                None => cfg.rungs.command(name),
            };
            out.push_str(&format!("{name}: {}\n", command.unwrap_or("(unset)")));
        }
        out.push_str(&format!("timeout: {}\n", cfg.rungs.timeout));
        let pools = if cfg.rungs.pools.is_empty() {
            "(any)".to_owned()
        } else {
            cfg.rungs.pools.join(", ")
        };
        out.push_str(&format!("pools: {pools}\n"));
        Ok(out)
    }

    /// Set rung `rung` to `command` in `[pump.rungs]`, or in
    /// `[pump.targets.<target>.rungs]` when `target` is given.
    ///
    /// Fails on an unknown rung name, a blank command, or an unknown target.
    pub fn set(
        project_root: &Path,
        rung: &str,
        command: &str,
        target: Option<&str>,
    ) -> Result<(), VivariumError> {
        if !RUNG_NAMES.contains(&rung) {
            return Err(VivariumError::Config(format!(
                "unknown rung `{rung}`; expected one of {}",
                RUNG_NAMES.join(", ")
            )));
        }
        if command.trim().is_empty() {
            return Err(VivariumError::Config(
                "the rung command must not be empty".into(),
            ));
        }
        let path = project_root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
        config_edit::apply(&path, |doc| {
            let mut keys = vec!["pump"];
            if let Some(id) = target {
                let known = doc
                    .get("pump")
                    .and_then(|pump| pump.get("targets"))
                    .and_then(|targets| targets.get(id))
                    .is_some();
                if !known {
                    return Err(VivariumError::Config(format!("unknown pump target `{id}`")));
                }
                keys.extend(["targets", id]);
            }
            keys.push("rungs");
            let table = config_edit::table_at(doc.as_table_mut(), &keys)?;
            table[rung] = toml_edit::value(command);
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "rungs_test.rs"]
mod tests;
