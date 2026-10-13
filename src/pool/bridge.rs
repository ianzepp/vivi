//! The MCP bridge grant a dispatch mints for a job.
//!
//! When a pool's `[pools.<name>.bridge]` is enabled, every dispatch on it
//! mints one project API key through the dispatching mailspace's own member
//! identity (see [`crate::sync::keys`]), labelled `pool-<pool>-<job>`. The
//! raw key travels only inside the job archive (see [`crate::pool::job`]);
//! the runner reads it and gives it to the agent as `VIVI_MCP_API_KEY`. The
//! key is never printed and never stored; only its key id is. A dispatch
//! that fails after the mint revokes the key.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::pool::github::Secret;
use crate::sync::SyncSettings;
use crate::sync::identity::SyncIdentity;
use crate::sync::keys;

/// The longest label the project server accepts.
const LABEL_MAX: usize = 64;

/// One minted bridge grant: the raw key, its server record, and the server
/// and project the agent will bridge to. The raw key never prints
/// ([`Secret`] redacts it).
#[derive(Debug, Clone)]
pub struct Grant {
    pub key: Secret,
    pub key_id: String,
    pub label: String,
    pub server: String,
    pub project_id: String,
}

/// The mint label for one job: `pool-<pool>-<job>`, at most
/// [`LABEL_MAX`] characters — an over-long pool name is truncated from the
/// right so the job id always survives whole.
#[must_use]
pub fn label_for(pool: &str, job: &str) -> String {
    let tail = format!("-{job}");
    let room = LABEL_MAX.saturating_sub("pool-".len() + tail.len());
    let pool = pool.chars().take(room).collect::<String>();
    format!("pool-{pool}{tail}")
}

/// Mint the grant for one job on `pool` of `space`: one project API key,
/// authorized by this mailspace's member identity. A mailspace without a
/// `[sync]` table is refused with the [`SyncSettings::resolve`] error before
/// anything is minted.
///
/// # Errors
/// Returns the resolve error, the identity error, or the mint error.
pub fn mint_for_job(space: &Mailspace, pool: &str, job: &str) -> Result<Grant, VivariumError> {
    let settings = SyncSettings::resolve(space)?;
    let identity = SyncIdentity::ensure(&space.dir)?;
    let label = label_for(pool, job);
    let minted = keys::mint_key(&settings, &identity, &label)?;
    Ok(Grant {
        key: Secret::new(minted.key),
        key_id: minted.record.key_id,
        label,
        server: settings.server.clone(),
        project_id: settings.project,
    })
}

/// Take a minted grant back: revoke its key on the server. The dispatch
/// failed after the mint, so the key must not outlive the attempt.
///
/// # Errors
/// Returns the resolve error, the identity error, or the revoke error.
pub fn revoke_grant(space: &Mailspace, grant: &Grant) -> Result<(), VivariumError> {
    let settings = SyncSettings::resolve(space)?;
    let identity = SyncIdentity::ensure(&space.dir)?;
    keys::revoke_key(&settings, &identity, &grant.key_id)
}

/// The MCP configuration the job archive carries when a grant exists: it
/// names the key only by reference (`${VIVI_MCP_API_KEY}`, the expansion a
/// harness such as Claude Code applies in an MCP config), never the key
/// itself. The runner copies it into the agent's home as `mcp.json`; a
/// harness template names the file with the `{mcp_config}` placeholder.
#[must_use]
pub fn mcp_config_json(server: &str, project_id: &str) -> String {
    serde_json::json!({
        "mcpServers": {
            "vivi": {
                "command": "vivi",
                "args": ["mcp"],
                "env": {
                    "VIVI_MCP_SERVER": server,
                    "VIVI_MCP_PROJECT_ID": project_id,
                    "VIVI_MCP_API_KEY": "${VIVI_MCP_API_KEY}",
                },
            },
        },
    })
    .to_string()
}

/// The brief a bridge worker gets instead of the task body: a pointer at the
/// task and at the vivi tools that read it and report on it. The task text
/// stays on the server; the file names the handle and the tools.
#[must_use]
pub fn bridge_brief(handle: &str, subject: &str) -> String {
    format!(
        "Your task handle is {handle} ({subject}). Read it with the vivi tools: `show` for the \
         task, `thread` for the conversation. Do the work, then report through the vivi tools: \
         `reply` with your receipt (what you changed, the checks you ran and how they ended, what \
         you left undone), and `done` when the task says so. The text of the task is on the \
         server, not in this file. If the vivi tools are not reachable, say so on standard \
         output and stop."
    )
}

#[cfg(test)]
#[path = "bridge_test.rs"]
mod tests;
