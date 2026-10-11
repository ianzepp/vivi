//! Project letter sync: `vivi sync` pushes local `.eml` letters to a project
//! server and pulls the letters other members pushed, byte for byte.
//!
//! The `.eml` is the unit of sync for every vivi type (mail, task, need,
//! want). A letter is immutable, so its hash (`sha256` of the bytes, the
//! store's `content_id`) is the same on every machine. Push sends the stored
//! file as-is; pull verifies the hash and ingests the bytes verbatim under
//! the envelope id. Tokens are minted per invocation and never persisted.
//! The wire is the "Letter wire" section of the server's phase-03 design.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;
use crate::mailspace::Mailspace;

pub mod http;
pub mod link;
pub mod pull;
pub mod push;
pub mod select;
pub mod state;
pub mod transport;

/// Remote-binding provider for letters synced with a project server.
pub const PROVIDER: &str = "vivi-project";

/// Fixed `remote_uidvalidity`: a project id never needs a validity epoch.
pub const UIDVALIDITY: u32 = 1;

/// Largest letter the server accepts (64 KiB, 413 above it).
pub const MAX_LETTER_BYTES: u64 = 64 * 1024;

/// Sync settings (the `[sync]` table in `mailspace.toml`).
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
    /// Local role that receives pulled letters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    /// Local role -> wire name, for letters addressed to a remote member.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
}

/// `[sync]` validated for one command: every key the command needs is set.
#[derive(Debug, Clone)]
pub struct SyncSettings {
    pub server: String,
    pub project: String,
    pub name: String,
    /// Canonical local role receiving pulled letters; resolved only for
    /// commands that pull.
    pub recipient: Option<String>,
    pub names: BTreeMap<String, String>,
}

impl SyncSettings {
    /// Validate `[sync]`. `pulls` also requires `recipient`, which must name
    /// a local role.
    ///
    /// # Errors
    /// Returns a config error naming the first missing or blank key.
    pub fn resolve(mailspace: &Mailspace, pulls: bool) -> Result<Self, VivariumError> {
        let config = mailspace.config.sync.as_ref().ok_or_else(|| {
            VivariumError::Config(
                "no [sync] table in mailspace.toml; add server, project, name, and recipient"
                    .into(),
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
        let recipient = if pulls {
            let role = required(
                config.recipient.as_deref(),
                "recipient",
                "the local role that receives pulled letters",
            )?;
            let canonical = mailspace
                .resolve_identity(&role)
                .map_err(|e| VivariumError::Config(format!("[sync] recipient: {e}")))?;
            Some(canonical)
        } else {
            None
        };
        if let Some((role, _)) = config.names.iter().find(|(_, wire)| wire.trim().is_empty()) {
            return Err(VivariumError::Config(format!(
                "[sync.names] {role} is blank; set it to the member's registered name"
            )));
        }
        Ok(Self {
            server: server.trim_end_matches('/').to_string(),
            project,
            name,
            recipient,
            names: config.names.clone(),
        })
    }

    /// The wire name a local account maps to: the `[sync.names]` entry for
    /// the account's role, else this checkout's own name.
    #[must_use]
    pub fn wire_name(&self, mailspace: &Mailspace, account: &str) -> String {
        let canonical = mailspace.resolve_identity(account).unwrap_or_default();
        [account, canonical.as_str()]
            .into_iter()
            .find_map(|role| self.names.get(role))
            .map_or_else(|| self.name.clone(), |wire| wire.trim().to_string())
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
#[path = "sync/sync_test.rs"]
mod tests;
