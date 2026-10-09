//! Chat client foundation: the `[chat]` config table, identity mint, and the
//! local state store under `<project>/.vivi/chat/`.

pub mod identity;
pub mod state;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::VivariumError;

/// Placeholder server URL shown when `[chat] server` is unset.
pub const DEFAULT_SERVER: &str = "https://vivi-chat.example";

/// Chat client settings (the `[chat]` table in `mailspace.toml`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ChatConfig {
    /// Base URL of the chat server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Whether `vivi board` may show the unread-chat badge.
    #[serde(default = "default_notify")]
    pub notify: bool,
    /// Seat label override (default is `<short-host>/<project>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

fn default_notify() -> bool {
    true
}

/// Chat home inside a mailspace: `<project>/.vivi/chat/`.
#[must_use]
pub fn chat_dir(vivi_dir: &Path) -> PathBuf {
    vivi_dir.join("chat")
}

/// Create the chat dir (0700) with a self-gitignore so nothing under it can
/// ever be committed.
///
/// # Errors
/// Returns an error if the directory or `.gitignore` cannot be created.
pub fn ensure_chat_dir(dir: &Path) -> Result<(), VivariumError> {
    crate::store::secure_create_dir_all(dir)?;
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "*\n")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "chat/chat_test.rs"]
mod tests;
