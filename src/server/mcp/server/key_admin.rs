//! The host key and grant admin tools (`key_mint`, `key_list`, `key_revoke`,
//! `machine_add`, `grant_add`, `grant_remove`) over the one server key store,
//! and the argument parsing they share.

use serde_json::{Value, json};

use crate::server::AppState;
use crate::server::grants::{GrantLevel, GrantRecord, GrantsError};
use crate::server::mcp::tools::{ToolError, opt_str, req_str};

use super::HOST_PRINCIPAL;

/// The key and grant admin tools over the one key store. A revocation
/// also retires the per-project mirror of a self-service key (rooms auth
/// and the letter wire read that file with no grant check), in every
/// project the key's principal is granted on.
pub(super) fn key_admin(
    state: &AppState,
    name: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    match name {
        "key_mint" => key_mint(state, arguments),
        "key_list" => Ok(json!({ "principals": state.server_keys.list() })),
        "key_revoke" => key_revoke(state, arguments),
        "machine_add" => machine_add(state, arguments),
        "grant_add" => grant_add(state, arguments),
        "grant_remove" => grant_remove(state, arguments),
        other => Err(ToolError::new(format!("unknown tool '{other}'"))),
    }
}

fn key_mint(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let host = opt_bool(arguments, "host", false)?;
    let principal = match opt_str(arguments, "principal")? {
        Some(principal) => principal.to_owned(),
        None if host => HOST_PRINCIPAL.to_owned(),
        None => {
            return Err(ToolError::new(
                "principal is required (or host: true for a host key)",
            ));
        }
    };
    let label = opt_str(arguments, "label")?
        .unwrap_or("server key")
        .to_owned();
    let grants = parse_grants(arguments.get("grants"))?;
    let minted = state
        .server_keys
        .mint(&principal, host, grants, &label, HOST_PRINCIPAL)
        .map_err(grants_error)?;
    Ok(json!({
        "principal": minted.principal,
        "key_id": minted.key_id,
        "key": minted.key,
        "label": minted.label,
        "host": minted.host,
        "grants": minted.grants,
        "created_at": minted.created_at,
        "note": "shown once; the server keeps only a digest",
    }))
}

fn key_revoke(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let store = &state.server_keys;
    let key_id = req_str(arguments, "key_id")?;
    // The mirror dies with the key, first: a storage failure then leaves the
    // key live everywhere rather than live in the mirror alone.
    if let Some((_, projects)) = store.holder_of_key(key_id) {
        for project in projects {
            state
                .keys
                .revoke_mirrored(&project, &[key_id])
                .map_err(mirror_error)?;
        }
    }
    store.revoke_key(key_id).map_err(grants_error)?;
    Ok(json!({ "revoked": key_id }))
}

fn machine_add(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let principal = req_str(arguments, "principal")?;
    let raw = req_str(arguments, "public_key")?;
    let key = crate::server::machine_auth::decode_key(raw)
        .ok_or_else(|| ToolError::new("public_key must be base64 of 32 bytes"))?;
    let encoded = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(key)
    };
    let added = state
        .server_keys
        .add_machine_key(principal, &encoded)
        .map_err(grants_error)?;
    Ok(json!({
        "principal": principal,
        "fingerprint": crate::server::machine_auth::fingerprint(&key),
        "added": added,
    }))
}

fn grant_add(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let principal = req_str(arguments, "principal")?;
    let raw = arguments
        .get("grant")
        .ok_or_else(|| ToolError::new("a grant object is required"))?;
    let grant = parse_grant(&resolve_new_grant(state, raw)?)?;
    state
        .server_keys
        .add_grant(principal, grant.clone())
        .map_err(grants_error)?;
    Ok(json!({ "principal": principal, "grant": grant }))
}

fn grant_remove(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let store = &state.server_keys;
    let principal = req_str(arguments, "principal")?;
    let project_id = req_str(arguments, "project_id")?;
    // This project's mirror entries of the principal's keys die with the
    // grant, first (same failure order as `key_revoke`); other projects'
    // mirrors are untouched. The project id is normalized exactly as
    // `remove_grant` normalizes it.
    if let Some(key_ids) = store.key_ids_of_principal(principal) {
        let ids: Vec<&str> = key_ids.iter().map(String::as_str).collect();
        state
            .keys
            .revoke_mirrored(project_id.trim(), &ids)
            .map_err(mirror_error)?;
    }
    store
        .remove_grant(principal, project_id)
        .map_err(|err| match err {
            GrantsError::NotFound => {
                ToolError::new(format!("{principal} holds no grant on {project_id}"))
            }
            other => grants_error(other),
        })?;
    Ok(json!({ "principal": principal, "removed": project_id }))
}

/// Complete a `grant_add` grant object: `project_id` may be a project id
/// or a hosted project's name (resolved to the id), and `member` may be
/// omitted when the project has exactly one registered member identity.
/// A member that is not registered on the project is refused here, naming
/// the registered ones, rather than at the grant's first use.
fn resolve_new_grant(state: &AppState, raw: &Value) -> Result<Value, ToolError> {
    let Some(object) = raw.as_object() else {
        return Err(ToolError::new("a grant must be an object"));
    };
    let named = object
        .get("project_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("a grant needs a project_id"))?;
    let project_id = match state.projects.load_meta_including_retired(named) {
        Ok(meta) => meta.project_id,
        Err(_) => crate::server::mcp::host::hosted_entry(state, named)
            .map(|meta| meta.project_id)
            .ok_or_else(|| ToolError::new(format!("no project with id or name '{named}'")))?,
    };
    let names: Vec<String> = state
        .identities
        .list(&project_id)
        .into_iter()
        .map(|identity| identity.name)
        .collect();
    let member = match object.get("member") {
        None | Some(Value::Null) => match names.as_slice() {
            [only] => only.clone(),
            [] => {
                return Err(ToolError::new(format!(
                    "project {project_id} has no registered member identities"
                )));
            }
            many => {
                return Err(ToolError::new(format!(
                    "project {project_id} has several registered members; name one of: {}",
                    many.join(", ")
                )));
            }
        },
        Some(Value::String(member)) => {
            if !names.iter().any(|name| name == member) {
                return Err(ToolError::new(format!(
                    "'{member}' is not a registered member on {project_id}; registered members: {}",
                    if names.is_empty() {
                        "(none)".to_owned()
                    } else {
                        names.join(", ")
                    }
                )));
            }
            member.clone()
        }
        Some(_) => return Err(ToolError::new("grant member must be a string")),
    };
    let mut resolved = object.clone();
    resolved.insert("project_id".into(), Value::String(project_id));
    resolved.insert("member".into(), Value::String(member));
    Ok(Value::Object(resolved))
}

/// A per-project mirror revocation failure: the storage error is logged
/// and the caller sees the shared refusal.
fn mirror_error(err: crate::server::keys::KeyError) -> ToolError {
    if let crate::server::keys::KeyError::Storage(message) = &err {
        tracing::error!("api key mirror failure: {message}");
    }
    ToolError::new("storage failure".to_owned())
}

fn grants_error(err: GrantsError) -> ToolError {
    let message = match err {
        GrantsError::InvalidPrincipal => {
            "principal must be 1-64 characters of letters, digits, '.', '-', '_' or '@'".to_owned()
        }
        GrantsError::InvalidLabel => {
            "label must be 1-64 characters with no control characters".to_owned()
        }
        GrantsError::InvalidGrant => {
            "a grant must name a project id and a member (1-64 characters), an optional \
             identity, and a level of member or operator"
                .to_owned()
        }
        GrantsError::HostWithGrants => "a host principal carries no project grants".to_owned(),
        GrantsError::HostMismatch => "that principal exists with a different host flag".to_owned(),
        GrantsError::PrincipalCollision(message) => message,
        GrantsError::LimitReached => "that principal already has 32 live keys".to_owned(),
        GrantsError::NotFound => "no such principal or key".to_owned(),
        GrantsError::Storage(message) => {
            tracing::error!("server key storage failure: {message}");
            "storage failure".to_owned()
        }
    };
    ToolError::new(message)
}

fn opt_bool(arguments: &Value, name: &str, default: bool) -> Result<bool, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(ToolError::new(format!("{name} must be true or false"))),
    }
}

/// One grant from tool arguments.
fn parse_grant(value: &Value) -> Result<GrantRecord, ToolError> {
    let Some(object) = value.as_object() else {
        return Err(ToolError::new("a grant must be an object"));
    };
    let project_id = object
        .get("project_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("a grant needs a project_id"))?;
    let member = object
        .get("member")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new("a grant needs a member"))?;
    let identity = match object.get("identity") {
        None | Some(Value::Null) => None,
        Some(Value::String(identity)) => Some(identity.clone()),
        Some(_) => return Err(ToolError::new("grant identity must be a string")),
    };
    let level = match object.get("level") {
        None | Some(Value::Null) => GrantLevel::Member,
        Some(Value::String(level)) => match level.to_ascii_lowercase().as_str() {
            "member" => GrantLevel::Member,
            "operator" => GrantLevel::Operator,
            _ => return Err(ToolError::new("grant level must be member or operator")),
        },
        Some(_) => return Err(ToolError::new("grant level must be member or operator")),
    };
    Ok(GrantRecord {
        project_id: project_id.to_owned(),
        member: member.to_owned(),
        identity,
        level,
    })
}

/// Grants from tool arguments (`key_mint`): absent means none.
fn parse_grants(value: Option<&Value>) -> Result<Vec<GrantRecord>, ToolError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items.iter().map(parse_grant).collect(),
        Some(_) => Err(ToolError::new("grants must be an array of grant objects")),
    }
}
