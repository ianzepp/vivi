//! Project letter sync: `vivi sync` pushes local `.eml` letters to a project
//! server and pulls the letters other members pushed, byte for byte.
//!
//! The `.eml` is the unit of sync for every vivi type (mail, task, need,
//! want). A letter is immutable, so its hash (`sha256` of the bytes, the
//! store's `content_id`) is the same on every machine. Push sends the stored
//! file as-is; pull verifies the hash and ingests the bytes verbatim under
//! the envelope id. Tokens are minted per invocation and never persisted.
//! The wire is the "Letter wire" section of the server's phase-03 design.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::mailspace::Mailspace;

pub mod fetch;
pub mod filer;
pub mod http;
pub mod link;
pub mod prune;
pub mod pull;
pub mod push;
pub mod refs;
pub mod select;
pub mod state;
pub mod transport;
pub mod window;

/// Remote-binding provider for letters synced with a project server.
pub const PROVIDER: &str = "vivi-project";

/// Fixed `remote_uidvalidity`: a project id never needs a validity epoch.
pub const UIDVALIDITY: u32 = 1;

/// Largest letter the server accepts (64 KiB, 413 above it).
pub const MAX_LETTER_BYTES: u64 = 64 * 1024;

/// Sync settings (the `[sync]` table in `mailspace.toml`).
///
/// Recipients are not configured: a letter's own To and Cc addresses name
/// them, and the domain of an address is the prefix of the mailspace it
/// belongs to. Keys older vivi versions wrote here (`recipient`,
/// `[sync.names]`) are ignored.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SyncConfig {
    /// Base URL of the project server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Server-side project id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// This checkout's registered identity name on the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `[sync]` validated for one command: every key the command needs is set.
#[derive(Debug, Clone)]
pub struct SyncSettings {
    pub server: String,
    pub project: String,
    pub name: String,
    /// This mailspace's id, sent when registering so the server can derive
    /// the address prefix.
    pub mailspace_id: String,
}

impl SyncSettings {
    /// Validate `[sync]` and read the mailspace id.
    ///
    /// # Errors
    /// Returns a config error naming the first missing or blank key.
    pub fn resolve(mailspace: &Mailspace) -> Result<Self, VivariumError> {
        let config = mailspace.config.sync.as_ref().ok_or_else(|| {
            VivariumError::Config(
                "no [sync] table in mailspace.toml; add server, project, and name".into(),
            )
        })?;
        let server = required(config.server.as_deref(), "server", "the project server URL")?;
        let project = required(
            config.project.as_deref(),
            "project",
            "the server-side project id",
        )?;
        if !project
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(VivariumError::Config(format!(
                "[sync] project '{project}' is not a project id (letters, digits, '-', '_', '.')"
            )));
        }
        let name = required(
            config.name.as_deref(),
            "name",
            "this checkout's identity name",
        )?;
        let mailspace_id = mailspace.mailspace_id();
        if mailspace_id.is_empty() {
            return Err(VivariumError::Config(
                "this mailspace has no id; reopen it with vivi so one is minted".into(),
            ));
        }
        Ok(Self {
            server: server.trim_end_matches('/').to_string(),
            project,
            name,
            mailspace_id: mailspace_id.to_string(),
        })
    }
}

impl SyncSettings {
    /// The address prefix of this mailspace: the first characters of its id,
    /// the domain every letter From address of this mailspace carries.
    #[must_use]
    pub fn address_prefix(&self) -> &str {
        self.mailspace_id
            .get(..crate::mailspace::PREFIX_LEN)
            .unwrap_or(&self.mailspace_id)
    }
}

fn required(value: Option<&str>, key: &str, what: &str) -> Result<String, VivariumError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
        .ok_or_else(|| {
            VivariumError::Config(format!(
                "[sync] {key} is not set; set {key} = \"...\" in mailspace.toml ({what})"
            ))
        })
}

/// Sync home inside a mailspace: `<project>/.vivi/sync/`.
#[must_use]
pub fn sync_dir(vivi_dir: &Path) -> PathBuf {
    vivi_dir.join("sync")
}

#[cfg(test)]
#[path = "sync/fake_server_test.rs"]
mod fake_server;

#[cfg(test)]
#[path = "sync/flow_test.rs"]
mod flow;

#[cfg(test)]
#[path = "sync/windows_test.rs"]
mod windows;

#[cfg(test)]
#[path = "sync/sync_test.rs"]
mod tests;
