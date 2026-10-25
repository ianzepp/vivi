//! Which projects the mind watches: the host registry's pumped roots, less
//! any the operator told the mind to ignore.
//!
//! The registry is the pump supervisor's `<config_dir>/vivi/pump-service.toml`
//! (see [`crate::registry`]). The mind only reads it; `vivi mind service
//! add|remove` changes the mind's own ignore list in its store, never the
//! registry.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::VivariumError;
use crate::registry;

/// One registered project as the mind sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    pub root: PathBuf,
    /// `pump = true` in the registry.
    pub pumped: bool,
    /// On the mind's ignore list.
    pub ignored: bool,
}

/// Every registry entry with the mind's view of it, ordered by root.
///
/// # Errors
/// Fails on an unreadable or invalid registry file.
pub fn list_at(
    registry_path: &Path,
    ignored: &BTreeSet<PathBuf>,
) -> Result<Vec<Watched>, VivariumError> {
    Ok(registry::load_at(registry_path)?
        .entries()
        .into_iter()
        .map(|entry| Watched {
            ignored: ignored.contains(&entry.root),
            pumped: entry.flags.pump,
            root: entry.root,
        })
        .collect())
}

/// The roots the mind probes: pumped and not ignored.
///
/// # Errors
/// Fails on an unreadable or invalid registry file.
pub fn watched_at(
    registry_path: &Path,
    ignored: &BTreeSet<PathBuf>,
) -> Result<Vec<PathBuf>, VivariumError> {
    Ok(list_at(registry_path, ignored)?
        .into_iter()
        .filter(|entry| entry.pumped && !entry.ignored)
        .map(|entry| entry.root)
        .collect())
}

#[cfg(test)]
#[path = "projects_test.rs"]
mod tests;
