//! Resolution for `vivi mcp`: flags, then environment, then the mailspace's
//! `[sync]` table.
//!
//! [`resolve`] is a pure function: the environment is read through a lookup
//! the caller supplies, and the mailspace is found through a closure that is
//! called only when the server or the project is still missing, so a run
//! whose flags and variables give everything opens, reads, and writes
//! nothing anywhere. The credential is an API key from the environment
//! only: the server's MCP endpoint takes API keys and nothing else.

use std::path::PathBuf;

use crate::error::VivariumError;

/// Environment variable holding the project server URL.
pub const ENV_SERVER: &str = "VIVI_MCP_SERVER";

/// Environment variable holding the server-side project id.
pub const ENV_PROJECT_ID: &str = "VIVI_MCP_PROJECT_ID";

/// Environment variable holding the API key. There is no flag for it on
/// purpose: a flag shows in a process list.
pub const ENV_API_KEY: &str = "VIVI_MCP_API_KEY";

/// The parsed `vivi mcp` flags, before environment and mailspace fallback.
#[derive(Debug, Clone, Default)]
pub struct McpFlags {
    /// `--server`: the project server URL.
    pub server: Option<String>,
    /// `--project-id`: the project's id on the server.
    pub project_id: Option<String>,
    /// `--project`: the mailspace root, consulted only for a `[sync]` table;
    /// never created.
    pub project: Option<PathBuf>,
}

/// What a mailspace offers resolution: its `[sync]` values.
#[derive(Debug, Clone)]
pub struct MailspaceSource {
    /// `[sync] server`, when set.
    pub server: Option<String>,
    /// `[sync] project`, when set.
    pub project: Option<String>,
}

/// Everything `vivi mcp` needs to serve.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The server URL, without a trailing slash.
    pub server: String,
    /// The project id, sent as the `project` argument of tools that take
    /// one; `None` when nothing named it.
    pub project: Option<String>,
    /// The API key, presented as the bearer as it is.
    pub api_key: String,
}

/// Resolve the bridge configuration: the server and the project each come
/// from a flag, else their variable, else the mailspace's `[sync]` table;
/// the API key comes from `VIVI_MCP_API_KEY` alone. An absent server or key
/// is refused with an error naming every place it could have come from,
/// before any request is made. The project is optional.
///
/// # Errors
/// Returns an error when the server or the API key cannot be resolved, or
/// the project id is not a project id. The mailspace closure is consulted
/// only when the server or the project is still missing; its error is
/// passed through.
pub fn resolve(
    flags: &McpFlags,
    env: impl Fn(&str) -> Option<String>,
    mailspace: impl Fn() -> Result<Option<MailspaceSource>, VivariumError>,
) -> Result<Resolved, VivariumError> {
    let env_server = non_empty(env(ENV_SERVER));
    let env_project = non_empty(env(ENV_PROJECT_ID));
    let api_key = non_empty(env(ENV_API_KEY));
    let needed = (flags.server.is_none() && env_server.is_none())
        || (flags.project_id.is_none() && env_project.is_none());
    let source = if needed { mailspace()? } else { None };
    let from_space = |pick: fn(&MailspaceSource) -> &Option<String>| {
        source.as_ref().and_then(|space| pick(space).clone())
    };
    let server = flags
        .server
        .clone()
        .or(env_server)
        .or_else(|| from_space(|s| &s.server));
    let project = flags
        .project_id
        .clone()
        .or(env_project)
        .or_else(|| from_space(|s| &s.project));
    let mut missing: Vec<String> = Vec::new();
    let server = required(
        server,
        &mut missing,
        "server: give --server <URL>, or set VIVI_MCP_SERVER, or set [sync] server in \
         mailspace.toml",
    );
    let api_key = required(
        api_key,
        &mut missing,
        "API key: set VIVI_MCP_API_KEY (the server MCP takes API keys only; mint one \
         with vivi sync api-key mint)",
    );
    match (server, api_key) {
        (Some(server), Some(api_key)) => Ok(Resolved {
            server: server.trim_end_matches('/').to_string(),
            project: project.as_deref().map(checked_project).transpose()?,
            api_key,
        }),
        _ => Err(unresolved(&missing)),
    }
}

/// An empty value counts as unset, whatever the caller's lookup returns.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// Keep `value` or record `text` among the missing pieces.
fn required(value: Option<String>, missing: &mut Vec<String>, text: &str) -> Option<String> {
    if value.is_none() {
        missing.push(text.to_string());
    }
    value
}

/// The project id must be letters, digits, `-`, `_` or `.`, as
/// [`crate::sync::SyncSettings::resolve`] checks.
fn checked_project(project: &str) -> Result<String, VivariumError> {
    let valid = project
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(project.to_string())
    } else {
        Err(VivariumError::Message(format!(
            "project id '{project}' is not a project id (letters, digits, '-', '_', '.')"
        )))
    }
}

/// One error listing every missing piece.
fn unresolved(missing: &[String]) -> VivariumError {
    VivariumError::Message(format!(
        "the MCP bridge is not configured:\n  {}",
        missing.join("\n  ")
    ))
}

#[cfg(test)]
#[path = "resolve_test.rs"]
mod tests;
