//! The `vivi pool list | show` view: one rendered row per pool.
//!
//! The rows carry exactly what those commands print — provider, slots, live
//! free slots, the bands with model and effort, and the harness models — so
//! another surface can render the same view without the CLI.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use super::config::PoolConfig;
use crate::error::VivariumError;
use crate::mailspace::Mailspace;

/// One pool as the `vivi pool list` and `vivi pool show` views render it.
/// This struct is also the body of both commands' `--json` output.
#[derive(Debug, Serialize)]
pub struct PoolRow {
    /// Pool name, the `[pools.<name>]` table key.
    pub name: String,
    pub provider: String,
    pub slots: u32,
    /// Live free slot count; `None` unless the view was asked to resolve it.
    pub free: Option<u32>,
    /// Per band key, the ordered model-and-effort choices that serve it.
    pub bands: BTreeMap<String, Vec<BandRow>>,
    /// The pool's `[..harness]` models; empty when no harness is set.
    pub harness_models: Vec<String>,
}

/// One model-and-reasoning choice a pool offers for a band.
#[derive(Debug, Serialize)]
pub struct BandRow {
    pub model: String,
    pub reasoning: String,
}

/// What `vivi pool show` resolves: the rendered [`PoolRow`] — the `--json`
/// body — plus the raw `[pools.<name>]` table for the fields the text view
/// prints and the row does not carry (`local.worktree_of`, `caps`).
#[derive(Debug)]
pub struct PoolShow {
    /// The rendered row.
    pub row: PoolRow,
    /// The pool table the row was rendered from.
    pub pool: PoolConfig,
}

/// Every pool in the mailspace, as `vivi pool list` renders it: config-table
/// order, `free` left unresolved.
///
/// # Errors
/// Returns an error when the mailspace cannot be discovered or its config
/// cannot be read.
pub fn list_view(project: Option<&Path>) -> Result<Vec<PoolRow>, VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    Ok(mailspace
        .config
        .pools
        .iter()
        .map(|(name, pool)| row(name, pool, None))
        .collect())
}

/// One pool by name, as `vivi pool show` renders it. With `resolved`, the row
/// carries the pool's live free slot count; the count falls back to `0` when
/// the status probe cannot run.
///
/// # Errors
/// Returns an error when the mailspace cannot be discovered or `name` is not
/// a configured pool.
pub fn show_view(
    project: Option<&Path>,
    name: &str,
    resolved: bool,
) -> Result<PoolShow, VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let pool = mailspace
        .config
        .pools
        .get(name)
        .ok_or_else(|| VivariumError::Message(format!("unknown pool '{name}'")))?;
    let free = resolved.then(|| live_free(name));
    Ok(PoolShow {
        row: row(name, pool, free),
        pool: pool.clone(),
    })
}

fn row(name: &str, pool: &PoolConfig, free: Option<u32>) -> PoolRow {
    PoolRow {
        name: name.to_string(),
        provider: pool.provider.clone(),
        slots: pool.slots,
        free,
        bands: pool
            .bands
            .entries
            .iter()
            .map(|(key, choices)| {
                let rows = choices
                    .iter()
                    .map(|c| BandRow {
                        model: c.model.clone(),
                        reasoning: c.reasoning.clone(),
                    })
                    .collect();
                (key.clone(), rows)
            })
            .collect(),
        harness_models: pool
            .harness
            .as_ref()
            .map_or_else(Vec::new, |h| h.models.clone()),
    }
}

fn live_free(name: &str) -> u32 {
    let out = std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "vivi".into()))
        .args(["pool", "status", "--pool", name, "--porcelain"])
        .output();
    let Ok(out) = out else {
        return 0;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            tokens.len() >= 5 && tokens[0] == "slot" && tokens[4] == "free"
        })
        .count() as u32
}

#[cfg(test)]
#[path = "config_view_test.rs"]
mod tests;
