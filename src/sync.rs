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
pub mod identity;
pub mod kept;
pub mod keys;
pub mod link;
pub mod lock;
pub mod prune;
pub mod pull;
pub mod push;
pub mod refs;
pub mod select;
pub mod state;
pub mod transport;
pub mod vouch;
pub mod window;

/// Remote-binding provider for letters synced with a project server.
pub const PROVIDER: &str = "vivi-project";

/// The validity remote bindings are written under when the server sends no
/// epoch: a project id never needs a validity epoch of its own. Once the
/// server does send an epoch, the epoch takes over as the validity, so a
/// wiped and restored server's reused sequence numbers cannot collide with
/// the previous log's bindings.
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
    /// What a push sends: every letter (`all`, the default) or only the
    /// letters addressed off this machine (`addressed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send: Option<String>,
}

/// What a push sends, from the `[sync]` `send` key: every letter, or only
/// the letters addressed off this machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SendFilter {
    /// Every letter (the default, and the behavior before the key existed).
    #[default]
    All,
    /// Only letters a recipient address off this machine names, plus event
    /// letters for items already bound to this project, so an event for
    /// work that has left the machine follows it.
    Addressed,
}

impl SendFilter {
    /// Read the `[sync]` `send` key: absent or blank is [`SendFilter::All`].
    ///
    /// # Errors
    /// Returns a config error naming the key `send` and its two values for
    /// any other value.
    pub fn parse(value: Option<&str>) -> Result<Self, VivariumError> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("all") => Ok(Self::All),
            Some("addressed") => Ok(Self::Addressed),
            Some(other) => Err(VivariumError::Config(format!(
                "[sync] send = \"{other}\" is not a send value; use send = \"all\" or \
                 send = \"addressed\" in mailspace.toml"
            ))),
        }
    }
}

/// `[sync]` validated for one command: every key the command needs is set.
#[derive(Debug, Clone)]
pub struct SyncSettings {
    pub server: String,
    pub project: String,
    pub name: String,
    /// What a push sends, from the `[sync]` `send` key.
    pub send: SendFilter,
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
                "no [sync] table in mailspace.toml; add server, project, and name. A new \
                 project is made with vivi sync create, which prints the project id: put the \
                 printed id in [sync] project, then run vivi sync join"
                    .into(),
            )
        })?;
        let server = required(
            config.server.as_deref(),
            "server",
            "the project server URL",
            "",
        )?;
        let project = required(
            config.project.as_deref(),
            "project",
            "the server-side project id",
            "vivi sync create makes a new project and prints its id: put the printed id in \
             [sync] project, then run vivi sync join",
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
            "",
        )?;
        let send = SendFilter::parse(config.send.as_deref())?;
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
            send,
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

fn required(
    value: Option<&str>,
    key: &str,
    what: &str,
    hint: &str,
) -> Result<String, VivariumError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
        .ok_or_else(|| {
            let base =
                format!("[sync] {key} is not set; set {key} = \"...\" in mailspace.toml ({what})");
            if hint.is_empty() {
                VivariumError::Config(base)
            } else {
                VivariumError::Config(format!("{base}; {hint}"))
            }
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
#[path = "sync/send_filter_test.rs"]
mod send_filter_test;

#[cfg(test)]
#[path = "sync/admission_test.rs"]
mod admission_test;

#[cfg(test)]
#[path = "sync/deps_test.rs"]
mod deps;

#[cfg(test)]
#[path = "sync/events_test.rs"]
mod events;

#[cfg(test)]
#[path = "sync/flow_test.rs"]
mod flow;

#[cfg(test)]
#[path = "sync/kept_flow_test.rs"]
mod kept_flow;

#[cfg(test)]
#[path = "sync/windows_test.rs"]
mod windows;

#[cfg(test)]
#[path = "sync/watch_test.rs"]
mod watch;

#[cfg(test)]
#[path = "sync/status_hint_test.rs"]
mod status_hint_test;

#[cfg(test)]
#[path = "sync/sync_test.rs"]
mod tests;

#[cfg(test)]
#[path = "sync/vouch_test.rs"]
mod vouch_test;

#[cfg(test)]
#[path = "sync/epoch_test.rs"]
mod epoch_test;

#[cfg(test)]
#[path = "sync/epoch_binding_test.rs"]
mod epoch_binding_test;

#[cfg(test)]
#[path = "sync/epoch_status_test.rs"]
mod epoch_status_test;
