//! The MCP bridge grant a dispatch mints for a job.
//!
//! When a pool's `[pools.<name>.bridge]` is enabled, every dispatch on it
//! mints one project API key through the dispatching mailspace's own member
//! identity (see [`crate::sync::keys`]), labelled `pool-<pool>-<job>`. The
//! raw key travels only inside the job archive (see [`crate::pool::job`]);
//! the runner reads it and gives it to the agent as `VIVI_MCP_API_KEY`. The
//! key is never printed and never stored; only its key id is. A dispatch
//! that fails after the mint revokes the key. A collected job's key is
//! revoked at collect; the keys of everything else a pool left behind are
//! swept by `pool destroy --all`.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::pool::github::Secret;
use crate::sync::SyncSettings;
use crate::sync::identity::SyncIdentity;
use crate::sync::keys;

/// The longest label the project server accepts.
const LABEL_MAX: usize = 64;

/// The length of the job id a mint label ends with
/// ([`crate::pool::dispatch::new_job_id`]).
const JOB_ID_LEN: usize = 12;

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

/// Whether a listed key label is one of `pool`'s minted bridge keys: the
/// pool's own `pool-<pool>-` prefix — the dash ends the pool, so pool `a`
/// never matches pool `a-b`'s keys — and then exactly a job id (twelve
/// hexadecimal characters, the shape of [`crate::pool::dispatch::new_job_id`])
/// and nothing else. A hand-minted label can only match by wild coincidence,
/// and another pool's never does.
#[must_use]
pub fn label_matches(label: &str, pool: &str) -> bool {
    let prefix = format!("pool-{pool}-");
    let Some(job) = label.strip_prefix(prefix.as_str()) else {
        return false;
    };
    job.len() == JOB_ID_LEN && job.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The exact command that revokes one key by id: what a warning hands a
/// human when a revoke the tool could not do is left to finish by hand.
#[must_use]
pub fn manual_revoke(key_id: &str) -> String {
    format!("vivi sync api-key revoke {key_id}")
}

/// Revoke the key one collected dispatch minted: the live key labelled
/// exactly [`label_for`]`(pool, job)` — never a sibling job's, never another
/// pool's, never a hand-minted one. `Ok(None)` when no live key carries the
/// label (the dispatch minted none, or a failed dispatch already revoked it).
///
/// # Errors
/// Returns the resolve error, the identity error, or the list or revoke
/// error; the caller turns a failure into a warning, never an error of the
/// collect.
pub fn revoke_for_job(
    space: &Mailspace,
    pool: &str,
    job: &str,
) -> Result<Option<String>, VivariumError> {
    let settings = SyncSettings::resolve(space)?;
    let identity = SyncIdentity::ensure(&space.dir)?;
    let label = label_for(pool, job);
    let mut revoked = None;
    for key in keys::list_keys(&settings, &identity)?
        .iter()
        .filter(|key| key.label == label)
    {
        keys::revoke_key(&settings, &identity, &key.key_id)?;
        revoked.get_or_insert_with(|| key.key_id.clone());
    }
    Ok(revoked)
}

/// Revoke every live key labelled for `pool` ([`label_matches`]) — the sweep
/// `pool destroy --all` runs, so nothing a lost or uncollected job minted
/// survives the pool. Each revoked key says so on standard error
/// (`revoked bridge key <key_id> (<label>)`); a key that cannot be revoked
/// only warns, naming the exact manual command. Returns the key ids revoked.
///
/// # Errors
/// Returns the resolve error, the identity error, or the list error; the
/// caller turns a failure into a warning, never an error of the destroy.
pub fn revoke_pool(space: &Mailspace, pool: &str) -> Result<Vec<String>, VivariumError> {
    let settings = SyncSettings::resolve(space)?;
    let identity = SyncIdentity::ensure(&space.dir)?;
    let mut revoked = Vec::new();
    for key in keys::list_keys(&settings, &identity)?
        .iter()
        .filter(|key| label_matches(&key.label, pool))
    {
        match keys::revoke_key(&settings, &identity, &key.key_id) {
            Ok(()) => {
                eprintln!("revoked bridge key {} ({})", key.key_id, key.label);
                revoked.push(key.key_id.clone());
            }
            Err(e) => eprintln!(
                "warning: bridge key {} ({}) could not be revoked: {e}; revoke it by hand: {}",
                key.key_id,
                key.label,
                manual_revoke(&key.key_id)
            ),
        }
    }
    Ok(revoked)
}

/// The MCP configuration the job archive carries when a grant exists: it
/// names the key only by reference (`${VIVI_MCP_API_KEY}`, the expansion a
/// harness such as Claude Code applies in an MCP config), never the key
/// itself. The runner copies it into the agent's home as `mcp.json`; a
/// harness template names the file with the `{mcp_config}` placeholder.
/// `VIVI_MCP_PROJECT_ID` is the project the bridge fills into the model's
/// tool calls that take one and carry none; it is not part of the URL.
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
