//! Resolution for `vivi mcp`: flags, then environment, then the mailspace's
//! `[sync]` table and its own key.
//!
//! [`resolve`] is a pure function: the environment is read through a lookup
//! the caller supplies, and the mailspace is found through a closure that is
//! called only when something is still missing, so a run whose flags and
//! variables give everything opens, reads, and writes nothing anywhere.
//! Reading a key file is left to the caller: resolution names the file only.

use std::path::PathBuf;

use crate::error::VivariumError;
use crate::sync::SendFilter;
use crate::sync::SyncSettings;
use crate::sync::identity::key_path;

/// Environment variable holding the project server URL.
pub const ENV_SERVER: &str = "VIVI_MCP_SERVER";

/// Environment variable holding the server-side project id.
pub const ENV_PROJECT_ID: &str = "VIVI_MCP_PROJECT_ID";

/// Environment variable holding the registered member name.
pub const ENV_NAME: &str = "VIVI_MCP_NAME";

/// Environment variable holding the member key file path.
pub const ENV_KEY_FILE: &str = "VIVI_MCP_KEY_FILE";

/// Environment variable holding a pre-shared API key. There is no flag for
/// it on purpose: a flag shows in a process list.
pub const ENV_API_KEY: &str = "VIVI_MCP_API_KEY";

/// The parsed `vivi mcp` flags, before environment and mailspace fallback.
#[derive(Debug, Clone, Default)]
pub struct McpFlags {
    /// `--server`: the project server URL.
    pub server: Option<String>,
    /// `--project-id`: the project's id on the server.
    pub project_id: Option<String>,
    /// `--name`: the registered member name.
    pub name: Option<String>,
    /// `--key-file`: a 32-byte member seed file.
    pub key_file: Option<PathBuf>,
    /// `--project`: the mailspace root, consulted only for a `[sync]` table
    /// and the mailspace's own key; never created.
    pub project: Option<PathBuf>,
}

/// What a mailspace offers resolution: its `[sync]` values and the
/// directory holding its own key.
#[derive(Debug, Clone)]
pub struct MailspaceSource {
    /// `[sync] server`, when set.
    pub server: Option<String>,
    /// `[sync] project`, when set.
    pub project: Option<String>,
    /// `[sync] name`, when set.
    pub name: Option<String>,
    /// The mailspace's `.vivi` directory, for its own key.
    pub vivi_dir: PathBuf,
}

/// A credential still to load: resolution itself reads no file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingCredential {
    /// A pre-shared API key, presented as the bearer as it is.
    ApiKey(String),
    /// A member key file named by flag or variable.
    KeyFile(PathBuf),
    /// The mailspace's own key, which this command never mints.
    MailspaceKey(PathBuf),
}

/// Everything `vivi mcp` needs to serve, with the credential left to load.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The server, project id, and member name; `mailspace_id` is empty
    /// because the bridge mints with name and key alone.
    pub settings: SyncSettings,
    /// The credential to load into a [`crate::mcp::Credential`].
    pub credential: PendingCredential,
}

/// Resolve the bridge configuration: each value comes from a flag, else its
/// variable, else the mailspace's `[sync]` table, and the credential comes
/// from an API key, else a key file with a name, else the mailspace's own
/// key. An absent value is refused with an error naming every place it
/// could have come from, before any request is made.
///
/// # Errors
/// Returns an error when the API key is combined with a member name or key
/// file, a value or credential cannot be resolved from anywhere, or the
/// project id is not a project id. The mailspace closure is consulted only
/// when something is still missing or the credential must come from its
/// key; its error is passed through.
pub fn resolve(
    flags: &McpFlags,
    env: impl Fn(&str) -> Option<String>,
    mailspace: impl Fn() -> Result<Option<MailspaceSource>, VivariumError>,
) -> Result<Resolved, VivariumError> {
    let env_server = non_empty(env(ENV_SERVER));
    let env_project = non_empty(env(ENV_PROJECT_ID));
    let env_name = non_empty(env(ENV_NAME));
    let api_key = non_empty(env(ENV_API_KEY));
    let key_file = flags
        .key_file
        .clone()
        .or_else(|| non_empty(env(ENV_KEY_FILE)).map(PathBuf::from));
    if api_key.is_some() && (key_file.is_some() || flags.name.is_some() || env_name.is_some()) {
        return Err(VivariumError::Message(
            "give an API key or a member name and key file, not both: VIVI_MCP_API_KEY \
             excludes --name and --key-file (VIVI_MCP_NAME and VIVI_MCP_KEY_FILE)"
                .into(),
        ));
    }
    let named = api_key.is_none();
    let needed = (flags.server.is_none() && env_server.is_none())
        || (flags.project_id.is_none() && env_project.is_none())
        || (named && (key_file.is_none() || (flags.name.is_none() && env_name.is_none())));
    let source = if needed { mailspace()? } else { None };
    let (server, project, name) = layered(flags, env_server, env_project, env_name, &source, named);
    let mut missing: Vec<String> = Vec::new();
    let server = required(
        server,
        &mut missing,
        "server: give --server <URL>, or set VIVI_MCP_SERVER, or set [sync] server in \
         mailspace.toml",
    );
    let project = required(
        project,
        &mut missing,
        "project id: give --project-id <ID>, or set VIVI_MCP_PROJECT_ID, or set [sync] \
         project in mailspace.toml",
    );
    let credential = pick_credential(&api_key, &key_file, &name, source.as_ref(), &mut missing);
    match (server, project, credential) {
        (Some(server), Some(project), Some(credential)) => Ok(Resolved {
            settings: SyncSettings {
                server: server.trim_end_matches('/').to_string(),
                project: checked_project(&project)?,
                name: name.unwrap_or_default(),
                send: SendFilter::All,
                mailspace_id: String::new(),
            },
            credential,
        }),
        _ => Err(unresolved(&missing)),
    }
}

/// Each value as flag, else variable, else the `[sync]` table.
fn layered(
    flags: &McpFlags,
    env_server: Option<String>,
    env_project: Option<String>,
    env_name: Option<String>,
    source: &Option<MailspaceSource>,
    named: bool,
) -> (Option<String>, Option<String>, Option<String>) {
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
    let name = if named {
        flags
            .name
            .clone()
            .or(env_name)
            .or_else(|| from_space(|s| &s.name))
    } else {
        None
    };
    (server, project, name)
}

/// The credential to load, or `None` with a line in `missing` saying what
/// would provide one.
fn pick_credential(
    api_key: &Option<String>,
    key_file: &Option<PathBuf>,
    name: &Option<String>,
    source: Option<&MailspaceSource>,
    missing: &mut Vec<String>,
) -> Option<PendingCredential> {
    if let Some(key) = api_key {
        return Some(PendingCredential::ApiKey(key.clone()));
    }
    if let Some(path) = key_file {
        if name.is_none() {
            missing.push(
                "name: a key file needs its member name; give --name <NAME>, or set \
                 VIVI_MCP_NAME, or set [sync] name in mailspace.toml"
                    .into(),
            );
            return None;
        }
        return Some(PendingCredential::KeyFile(path.clone()));
    }
    match source {
        Some(space) => {
            if name.is_none() {
                missing.push(
                    "name: the mailspace key needs its member name; give --name <NAME>, or \
                     set VIVI_MCP_NAME, or set [sync] name in mailspace.toml"
                        .into(),
                );
                return None;
            }
            Some(PendingCredential::MailspaceKey(key_path(&space.vivi_dir)))
        }
        None => {
            missing.push(
                "credential: set VIVI_MCP_API_KEY, or give --name and --key-file \
                 (VIVI_MCP_NAME and VIVI_MCP_KEY_FILE), or run vivi sync key so this \
                 mailspace has a key"
                    .into(),
            );
            None
        }
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
/// [`SyncSettings::resolve`] checks.
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
