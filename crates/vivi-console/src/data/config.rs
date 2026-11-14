//! The project's mailspace config, read without opening the mailspace.
//!
//! `Mailspace::open_root` mints a missing id and may write a `.gitignore`;
//! the console only reads the file, merged over its bases the way vivi does.

use std::path::Path;

use vivi_core::mailspace::{MAILSPACE_CONFIG, MailspaceConfig, inherit};
use vivi_error::VivariumError;

use super::store::mailspace_dir;

/// A pool as declared in the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredPool {
    pub name: String,
    /// The pool's `slots` cap.
    pub slots: u32,
}

/// A slot named by a `[[slot]]` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredSlot {
    pub name: String,
    /// The host the slot lives on.
    pub host: String,
    /// The pools that may use it: the declared ones, or every pool when the
    /// declaration names none.
    pub pools: Vec<String>,
}

/// Read and merge `.vivi/mailspace.toml` under `root`.
///
/// # Errors
/// Returns an error when the file is missing or does not parse.
pub fn load(root: &Path) -> Result<MailspaceConfig, VivariumError> {
    let path = mailspace_dir(root).join(MAILSPACE_CONFIG);
    let raw = std::fs::read_to_string(&path)?;
    inherit::load_config(&path, &raw)
}

/// The named slots a config declares, in config order.
#[must_use]
pub fn declared_slots(config: &MailspaceConfig) -> Vec<DeclaredSlot> {
    config
        .slots
        .iter()
        .map(|slot| DeclaredSlot {
            name: slot.name.clone(),
            host: slot.host.clone(),
            pools: if slot.pools.is_empty() {
                config.pools.keys().cloned().collect()
            } else {
                slot.pools.clone()
            },
        })
        .collect()
}

/// The pools a config declares, in name order, with their slot caps.
#[must_use]
pub fn declared_pools(config: &MailspaceConfig) -> Vec<DeclaredPool> {
    config
        .pools
        .iter()
        .map(|(name, pool)| DeclaredPool {
            name: name.clone(),
            slots: pool.slots,
        })
        .collect()
}
