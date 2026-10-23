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
    /// Seconds one rung run may take; unset means the next level up
    /// (workspace, then [`DEFAULT_RUNG_TIMEOUT`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Pools a no-model rung job may run on; empty means no pool can run it,
    /// so a set rung command with no pools never dispatches.
    pub pools: Vec<String>,
}

impl Default for RungsConfig {
    fn default() -> Self {
        Self {
            task: None,
            need: None,
            main: None,
            release: None,
            timeout: None,
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

impl super::PumpConfig {
    /// The rung settings a job for `target` actually uses: each command is the
    /// target's own else the workspace one, the timeout is the target's else
    /// the workspace's else [`DEFAULT_RUNG_TIMEOUT`] (always set in the result),
    /// and the pools are the target's when it names any, else the workspace's.
    #[must_use]
    pub fn effective_rungs(&self, target: Option<&str>) -> RungsConfig {
        let own = target.and_then(|id| self.targets.get(id)).map(|t| &t.rungs);
        let shared = &self.rungs;
        let pick = |name: &str| {
            own.and_then(|own| own.command(name))
                .or_else(|| shared.command(name))
                .map(str::to_owned)
        };
        RungsConfig {
            task: pick("task"),
            need: pick("need"),
            main: pick("main"),
            release: pick("release"),
            timeout: Some(
                own.and_then(|own| own.timeout)
                    .or(shared.timeout)
                    .unwrap_or(DEFAULT_RUNG_TIMEOUT),
            ),
            pools: own
                .filter(|own| !own.pools.is_empty())
                .map_or_else(|| shared.pools.clone(), |own| own.pools.clone()),
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
        let effective = cfg.effective_rungs(target);
        let mut out = String::new();
        for name in RUNG_NAMES {
            let command = effective.command(name);
            out.push_str(&format!("{name}: {}\n", command.unwrap_or("(unset)")));
        }
        out.push_str(&format!(
            "timeout: {}\n",
            effective.timeout.unwrap_or(DEFAULT_RUNG_TIMEOUT)
        ));
        let pools = if effective.pools.is_empty() {
            "(none: rung jobs cannot dispatch)".to_owned()
        } else {
            effective.pools.join(", ")
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
