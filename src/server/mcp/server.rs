//! The server-level MCP endpoint, `POST /v1/mcp` (wave 3 D3): one endpoint
//! for the whole server. Project and host MCP live here; chat rooms stay
//! on `POST /v1/rooms/{id}/mcp`. The Bearer is a server key from the one key store
//! ([`crate::grants::ServerKeyStore`]) or, when set, the host operator
//! token; a project identity token is refused with a message naming the
//! need for a server key.
//!
//! Every project tool takes a required `project` argument, resolved
//! strictly among the key's grants: an exact project id first, else a name
//! (a hosted project's root directory name, else its mailspace config
//! name). No grant, and an unknown project, are the same error so a key
//! cannot probe which projects exist — this grant check is the only wall
//! between tenants on a global endpoint, and it never forwards the raw
//! argument: dispatch always receives the resolved grant's own project id.
//! The grant's member must still be a registered identity, and operator
//! tools re-check today's operator rule per call (the shared project
//! dispatch in [`crate::mcp`] applies it).
//!
//! One audit line per write tool call ([`crate::audit`]); reads are not
//! logged. Writes are rate limited per (project, key) — `host` for host
//! tools — through the same [`RateLimiter`] the other routes use.

use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::AppState;
use crate::audit::{AuditEvent, AuditOutcome};
use crate::auth::constant_time_eq;
use crate::grants::{GrantLevel, GrantRecord, GrantsError};
use crate::identities::IdentityRecord;
use crate::keys::Caller;
use crate::projects::ProjectMeta;
use crate::projects_api::{invalid_bearer, missing_bearer, presented_bearer};
use crate::rooms::error_response;
use crate::safety::RateLimiter;

use super::tools::{ToolError, opt_str, req_str};
use super::{Surface, respond, tool, tool_result};

/// The built-in principal behind `HOST_OPERATOR_TOKEN`.
pub(crate) const HOST_PRINCIPAL: &str = "host";

pub(super) const INSTRUCTIONS: &str = "This is one vivi server's MCP endpoint. Every project \
tool takes a required `project`: the project id, or the name of a hosted project this key is \
granted on. Call `projects` first to list what this key may reach. Host tools (create_project, \
project_list, server_version, key and grant admin) need a host principal.";

/// What the Bearer resolved to on `/v1/mcp`.
#[derive(Debug, Clone)]
pub(crate) struct ServerPrincipal {
    pub(crate) principal: String,
    /// The matched server key, `None` for the built-in host principal.
    pub(crate) key_id: Option<String>,
    pub(crate) host: bool,
    pub(crate) grants: Vec<GrantRecord>,
}

/// `POST /v1/mcp` — Streamable HTTP MCP (JSON-RPC 2.0) for the whole
/// server, authenticated by a server key or the host operator token.
pub async fn server_mcp_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Some(bearer) = presented_bearer(&headers) else {
        return missing_bearer();
    };
    match authenticate(&state, bearer) {
        Ok(auth) => {
            respond(
                &Surface::Server {
                    state: &state,
                    auth: &auth,
                },
                &headers,
                body,
            )
            .await
        }
        Err(response) => response,
    }
}

/// Resolve the Bearer: the host operator token (constant-time), then a
/// live server key. Identity tokens never carry the `vivi_` key prefix, so
/// anything else is told that `/v1/mcp` needs a server key; an unknown
/// `vivi_` secret is the generic invalid bearer.
fn authenticate(state: &AppState, bearer: &str) -> Result<ServerPrincipal, Response> {
    if let Some(expected) = state.host_operator_token.as_deref()
        && constant_time_eq(bearer, expected)
    {
        return Ok(ServerPrincipal {
            principal: HOST_PRINCIPAL.to_owned(),
            key_id: None,
            host: true,
            grants: Vec::new(),
        });
    }
    if let Some(verified) = state.server_keys.verify(bearer) {
        return Ok(ServerPrincipal {
            principal: verified.principal,
            key_id: Some(verified.key_id),
            host: verified.host,
            grants: verified.grants,
        });
    }
    if !bearer.starts_with("vivi_") {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "identity tokens are not accepted on /v1/mcp; this endpoint needs a server key",
        ));
    }
    Err(invalid_bearer())
}

// ------------------------------------------------------------------ tools

const HOST_TOOLS: [&str; 8] = [
    "create_project",
    "key_mint",
    "key_list",
    "key_revoke",
    "grant_add",
    "grant_remove",
    "server_version",
    "project_list",
];

/// The `/v1/mcp` tool list: every project tool with `project` required,
/// then `projects`, then the host tools.
pub(super) fn tools_list() -> Value {
    let mut tools: Vec<Value> = super::tools_list()["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for entry in &mut tools {
        require_project_argument(entry);
    }
    tools.push(tool(
        "projects",
        "List the projects this key is granted on: {project_id, name?, mode, member, identity, \
         level} per live grant (a hosted project's name is its root directory name). A host \
         principal answers {host: true}.",
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    ));
    if let Some(host_tools) = super::host::tools_list()["tools"].as_array() {
        tools.extend(host_tools.iter().cloned());
    }
    tools.push(tool(
        "key_mint",
        "Host only. Mint a server key for `principal` (created when absent), carrying `grants` \
         ([{project_id, member, identity?, level}]). `host: true` mints a host principal, which \
         carries no grants; `principal` then defaults to `host`. The raw key is shown once.",
        json!({
            "type": "object",
            "properties": {
                "principal": { "type": "string", "description": "Principal id, such as member@<project_id>; required unless host: true" },
                "host": { "type": "boolean", "default": false },
                "grants": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "project_id": { "type": "string" },
                            "member": { "type": "string" },
                            "identity": { "type": "string" },
                            "level": { "type": "string", "enum": ["member", "operator"] }
                        },
                        "required": ["project_id", "member"],
                        "additionalProperties": false
                    }
                },
                "label": { "type": "string", "default": "server key" }
            },
            "additionalProperties": false
        }),
    ));
    tools.push(tool(
        "key_list",
        "Host only. List principals, their keys (digests only) and grants.",
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    ));
    tools.push(tool(
        "key_revoke",
        "Host only. Revoke one server key by id, on any principal. Already-revoked is success.",
        json!({
            "type": "object",
            "properties": { "key_id": { "type": "string" } },
            "required": ["key_id"],
            "additionalProperties": false
        }),
    ));
    tools.push(tool(
        "grant_add",
        "Host only. Add one grant to an existing principal. A host principal accepts none.",
        json!({
            "type": "object",
            "properties": {
                "principal": { "type": "string" },
                "grant": {
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "member": { "type": "string" },
                        "identity": { "type": "string" },
                        "level": { "type": "string", "enum": ["member", "operator"] }
                    },
                    "required": ["project_id", "member"],
                    "additionalProperties": false
                }
            },
            "required": ["principal", "grant"],
            "additionalProperties": false
        }),
    ));
    tools.push(tool(
        "grant_remove",
        "Host only. Remove the grant on one project from a principal.",
        json!({
            "type": "object",
            "properties": {
                "principal": { "type": "string" },
                "project_id": { "type": "string" }
            },
            "required": ["principal", "project_id"],
            "additionalProperties": false
        }),
    ));
    json!({ "tools": tools })
}

/// Give one tool schema a required `project` argument, keeping what is
/// there (including `additionalProperties: false`).
fn require_project_argument(entry: &mut Value) {
    let Some(schema) = entry.get_mut("inputSchema").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert(
            "project".to_owned(),
            json!({
                "type": "string",
                "description": "Project id, or the name of a hosted project this key is granted on"
            }),
        );
    }
    let mut required = vec![json!("project")];
    required.extend(
        schema
            .get("required")
            .and_then(Value::as_array)
            .map(Vec::clone)
            .unwrap_or_default(),
    );
    schema.insert("required".to_owned(), Value::Array(required));
}

// ------------------------------------------------------------------ calls

pub(super) async fn call_tool(
    state: &AppState,
    auth: &ServerPrincipal,
    name: &str,
    arguments: Value,
) -> Result<Value, Response> {
    if HOST_TOOLS.contains(&name) {
        return host_tool(state, auth, name, arguments).await;
    }
    if name == "projects" {
        return Ok(tool_result(projects_answer(state, auth).await?, false));
    }
    project_tool(state, auth, name, arguments).await
}

/// One grant the call resolved to, ready to dispatch.
struct Resolved {
    project_id: String,
    identity: IdentityRecord,
    /// The vivi identity the key acts as on this project.
    acting_as: Option<String>,
}

async fn project_tool(
    state: &AppState,
    auth: &ServerPrincipal,
    name: &str,
    arguments: Value,
) -> Result<Value, Response> {
    let write = is_write(name, &arguments);
    // (b) the project argument. A non-string counts as missing, as the
    // wire contract pins it.
    let Some(named) = arguments
        .get("project")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(refused(state, auth, None, name, write, "project is required").await);
    };
    // (c) resolve among the key's grants. Blocking: project metadata reads.
    let resolve_state = state.clone();
    let grants = auth.grants.clone();
    let resolve_named = named.clone();
    let resolved = match tokio::task::spawn_blocking(move || {
        resolve_grant(&resolve_state, &grants, &resolve_named)
    })
    .await
    {
        Ok(Ok(resolved)) => resolved,
        Ok(Err(message)) => {
            return Ok(refused(state, auth, None, name, write, &message).await);
        }
        Err(err) => return Err(internal(state, auth, None, name, write, err).await),
    };
    let Resolved {
        project_id,
        identity,
        acting_as,
    } = resolved;
    // (7) writes are limited per (project, key).
    if write && let Err(message) = allow_write(state, &project_id, auth) {
        return Ok(refused(state, auth, Some(&project_id), name, write, &message).await);
    }
    // The tool sees its own arguments only; the resolved project never
    // rides through as the raw name.
    let mut arguments = arguments;
    if let Some(object) = arguments.as_object_mut() {
        object.remove("project");
    }
    let caller = Caller {
        identity,
        acting_as,
        via_key: true,
    };
    // The unlimited limiter: this route counts writes itself, keyed by
    // key, so the inner per-identity check must not double-count.
    match super::dispatch_tool(
        state,
        RateLimiter::new(None),
        &project_id,
        &caller,
        name,
        arguments,
    )
    .await
    {
        Ok(Ok(value)) => {
            if write {
                audit(
                    state,
                    auth,
                    Some(&project_id),
                    name,
                    AuditOutcome::Ok,
                    &summarize_ok(name, &value),
                )
                .await;
            }
            Ok(tool_result(value, false))
        }
        Ok(Err(err)) => {
            if write {
                audit(
                    state,
                    auth,
                    Some(&project_id),
                    name,
                    AuditOutcome::Refused,
                    &err.message,
                )
                .await;
            }
            Ok(tool_result(json!({ "error": err.message }), true))
        }
        Err(response) => {
            if write {
                audit(
                    state,
                    auth,
                    Some(&project_id),
                    name,
                    AuditOutcome::Error,
                    "the tool task failed",
                )
                .await;
            }
            Err(response)
        }
    }
}

/// Resolve `named` to one grant of `grants`: an exact project id first,
/// else a name among the hosted grants. No match and unknown project share
/// one text. The returned project id always comes from the grant, never
/// from `named`.
fn resolve_grant(
    state: &AppState,
    grants: &[GrantRecord],
    named: &str,
) -> Result<Resolved, String> {
    if let Some(grant) = grants.iter().find(|grant| grant.project_id == named) {
        return live_grant(state, grant);
    }
    let mut by_name: Vec<&GrantRecord> = Vec::new();
    for grant in grants {
        let Ok(meta) = state.projects.load_meta(&grant.project_id) else {
            continue;
        };
        if hosted_name(&meta).as_deref() == Some(named) {
            by_name.push(grant);
        }
    }
    match by_name.as_slice() {
        [] => Err(format!("this key has no grant on {named}")),
        [only] => live_grant(state, only),
        many => {
            let ids: Vec<&str> = many.iter().map(|grant| grant.project_id.as_str()).collect();
            Err(format!(
                "two grants of this key resolve to the name '{named}': {}",
                ids.join(" and ")
            ))
        }
    }
}

/// A grant whose project still exists and whose member is still a
/// registered identity. A project that is gone reads as ungranted (the
/// exact-id error names the grant's own project id).
fn live_grant(state: &AppState, grant: &GrantRecord) -> Result<Resolved, String> {
    let meta = state
        .projects
        .load_meta(&grant.project_id)
        .map_err(|_| format!("this key has no grant on {}", grant.project_id))?;
    if dead_chat(&meta) {
        return Err(format!("this key has no grant on {}", grant.project_id));
    }
    let identity = state
        .identities
        .by_name(&grant.project_id, &grant.member)
        .ok_or_else(|| {
            format!(
                "the grant's member '{}' is no longer a registered identity on {}",
                grant.member, grant.project_id
            )
        })?;
    Ok(Resolved {
        project_id: grant.project_id.clone(),
        identity,
        acting_as: grant.identity.clone(),
    })
}

/// A chat room that closed or expired; durable projects are never over.
fn dead_chat(meta: &ProjectMeta) -> bool {
    meta.is_chat() && meta.is_over()
}

/// A hosted project's name: its root directory name, else its mailspace
/// config `name`. Durable and chat projects resolve by id only.
pub(super) fn hosted_name(meta: &ProjectMeta) -> Option<String> {
    if dead_chat(meta) {
        return None;
    }
    let root = meta.mailspace_root.as_ref()?;
    if let Some(name) = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
    {
        return Some(name.to_owned());
    }
    vivi::mailspace::Mailspace::open_root(root.clone())
        .ok()
        .map(|ms| ms.config.name)
        .filter(|name| !name.trim().is_empty())
}

/// The `projects` tool: one entry per live grant; a host principal answers
/// `{host: true}`.
async fn projects_answer(state: &AppState, auth: &ServerPrincipal) -> Result<Value, Response> {
    if auth.host {
        return Ok(json!({ "host": true }));
    }
    let resolve_state = state.clone();
    let grants = auth.grants.clone();
    match tokio::task::spawn_blocking(move || {
        let mut items = Vec::new();
        for grant in grants {
            let Ok(meta) = resolve_state.projects.load_meta(&grant.project_id) else {
                continue; // a grant whose project no longer exists
            };
            if dead_chat(&meta) {
                continue;
            }
            let mut item = json!({
                "project_id": grant.project_id,
                "mode": if meta.is_hosted() {
                    "hosted"
                } else if meta.is_chat() {
                    "chat"
                } else {
                    "durable"
                },
                "member": grant.member,
                "identity": grant.identity,
                "level": serde_json::to_value(grant.level).unwrap_or(json!("member")),
            });
            if let Some(name) = hosted_name(&meta) {
                item["name"] = json!(name);
            }
            items.push(item);
        }
        items
    })
    .await
    {
        Ok(items) => Ok(Value::Array(items)),
        Err(err) => Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            &format!("projects task failed: {err}"),
        )),
    }
}

// ------------------------------------------------------------- host tools

async fn host_tool(
    state: &AppState,
    auth: &ServerPrincipal,
    name: &str,
    arguments: Value,
) -> Result<Value, Response> {
    // The read host tools: `key_list` names keys, the two D4 reads answer
    // server facts; none is audited or rate limited.
    let write = !matches!(name, "key_list" | "server_version" | "project_list");
    if !auth.host {
        let message = format!("{name} is a host tool; this key is not a host principal");
        if write {
            audit(state, auth, None, name, AuditOutcome::Refused, &message).await;
        }
        return Ok(tool_result(json!({ "error": message }), true));
    }
    if name == "server_version" {
        return Ok(tool_result(super::host::server_version(), false));
    }
    if name == "project_list" {
        let list_state = state.clone();
        return match tokio::task::spawn_blocking(move || super::host::project_list(&list_state))
            .await
        {
            Ok(value) => Ok(tool_result(value, false)),
            Err(err) => Err(internal(state, auth, None, name, write, err).await),
        };
    }
    if name == "create_project" {
        if let Err(message) = allow_write(state, HOST_PRINCIPAL, auth) {
            return Ok(refused(state, auth, None, name, true, &message).await);
        }
        return match super::host::create_project(state, &arguments).await {
            Ok(value) => {
                audit(
                    state,
                    auth,
                    None,
                    name,
                    AuditOutcome::Ok,
                    &summarize_ok(name, &value),
                )
                .await;
                Ok(tool_result(value, false))
            }
            Err(err) => {
                audit(state, auth, None, name, AuditOutcome::Refused, &err.message).await;
                Ok(tool_result(json!({ "error": err.message }), true))
            }
        };
    }
    if write && let Err(message) = allow_write(state, HOST_PRINCIPAL, auth) {
        return Ok(refused(state, auth, None, name, write, &message).await);
    }
    let admin_state = state.clone();
    let owned_name = name.to_owned();
    let result =
        match tokio::task::spawn_blocking(move || key_admin(&admin_state, &owned_name, &arguments))
            .await
        {
            Ok(result) => result,
            Err(err) => {
                let message = format!("key admin task failed: {err}");
                if write {
                    audit(state, auth, None, name, AuditOutcome::Error, &message).await;
                }
                return Err(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "storage failure",
                ));
            }
        };
    match result {
        Ok(value) => {
            if write {
                audit(
                    state,
                    auth,
                    None,
                    name,
                    AuditOutcome::Ok,
                    &summarize_ok(name, &value),
                )
                .await;
            }
            Ok(tool_result(value, false))
        }
        Err(err) => {
            if write {
                audit(state, auth, None, name, AuditOutcome::Refused, &err.message).await;
            }
            Ok(tool_result(json!({ "error": err.message }), true))
        }
    }
}

/// The key and grant admin tools over the one key store. A revocation
/// also retires the per-project mirror of a self-service key (rooms auth
/// and the letter wire read that file with no grant check), in every
/// project the key's principal is granted on.
fn key_admin(state: &AppState, name: &str, arguments: &Value) -> Result<Value, ToolError> {
    let store = &state.server_keys;
    match name {
        "key_mint" => {
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
            let minted = store
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
        "key_list" => Ok(json!({ "principals": store.list() })),
        "key_revoke" => {
            let key_id = req_str(arguments, "key_id")?;
            // The mirror dies with the key, first: a storage failure then
            // leaves the key live everywhere rather than live in the
            // mirror alone.
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
        "grant_add" => {
            let principal = req_str(arguments, "principal")?;
            let grant = arguments
                .get("grant")
                .ok_or_else(|| ToolError::new("a grant object is required"))
                .and_then(parse_grant)?;
            store
                .add_grant(principal, grant.clone())
                .map_err(grants_error)?;
            Ok(json!({ "principal": principal, "grant": grant }))
        }
        "grant_remove" => {
            let principal = req_str(arguments, "principal")?;
            let project_id = req_str(arguments, "project_id")?;
            // This project's mirror entries of the principal's keys die
            // with the grant, first (same failure order as `key_revoke`);
            // other projects' mirrors are untouched. The project id is
            // normalized exactly as `remove_grant` normalizes it.
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
        other => Err(ToolError::new(format!("unknown tool '{other}'"))),
    }
}

/// A per-project mirror revocation failure: the storage error is logged
/// and the caller sees the shared refusal.
fn mirror_error(err: crate::keys::KeyError) -> ToolError {
    if let crate::keys::KeyError::Storage(message) = &err {
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

// -------------------------------------------------------- audit and limits

/// Whether one tool call writes (so it is audited and rate limited). The
/// action-gated tools write only on their write actions.
fn is_write(name: &str, arguments: &Value) -> bool {
    let action = arguments.get("action").and_then(Value::as_str);
    match name {
        "send" | "reply" | "done" | "reopen" | "mark" | "unmark" | "depend" | "pump_control"
        | "pump_send" | "pump_action" | "pump_reset" => true,
        "memo" => action == Some("save"),
        "pump_rungs" => action == Some("set"),
        "pool_bands" => matches!(action, Some("set" | "unset")),
        "config_set" | "config_unset" | "pool_allow_set" | "target_add" | "target_remove" => true,
        "config_migrate" => match arguments.get("write") {
            Some(Value::Bool(flag)) => *flag,
            None | Some(Value::Null) => false,
            Some(_) => true,
        },
        _ => false,
    }
}

/// Count one write against (project-or-`host`, key).
fn allow_write(state: &AppState, bucket: &str, auth: &ServerPrincipal) -> Result<(), String> {
    let seat = auth
        .key_id
        .clone()
        .unwrap_or_else(|| auth.principal.clone());
    let mut hasher = DefaultHasher::new();
    seat.hash(&mut hasher);
    if state.limiter.allow_send(bucket, hasher.finish() as u32) {
        Ok(())
    } else {
        Err(format!(
            "over {} writes/min for this key",
            state.limiter.limit().unwrap_or(0)
        ))
    }
}

/// A refused tool call: a tool error, audited when the call is a write.
async fn refused(
    state: &AppState,
    auth: &ServerPrincipal,
    project_id: Option<&str>,
    tool: &str,
    write: bool,
    message: &str,
) -> Value {
    if write {
        audit(
            state,
            auth,
            project_id,
            tool,
            AuditOutcome::Refused,
            message,
        )
        .await;
    }
    tool_result(json!({ "error": message }), true)
}

/// An infrastructure failure: audited as an error when the call is a
/// write, then the 500 the other routes use.
async fn internal(
    state: &AppState,
    auth: &ServerPrincipal,
    project_id: Option<&str>,
    tool: &str,
    write: bool,
    err: tokio::task::JoinError,
) -> Response {
    if write {
        audit(
            state,
            auth,
            project_id,
            tool,
            AuditOutcome::Error,
            &format!("the tool task failed: {err}"),
        )
        .await;
    }
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "storage failure",
    )
}

/// One audit line for a write tool call, whatever came of it.
async fn audit(
    state: &AppState,
    auth: &ServerPrincipal,
    project_id: Option<&str>,
    tool: &str,
    outcome: AuditOutcome,
    summary: &str,
) {
    let log = state.audit.clone();
    let event = AuditEvent {
        key_id: auth.key_id.clone(),
        principal: Some(auth.principal.clone()),
        project_id: project_id.map(str::to_owned),
        tool: tool.to_owned(),
        outcome,
        summary: cap_summary(summary),
    };
    if let Ok(Err(err)) = tokio::task::spawn_blocking(move || log.record(event)).await {
        tracing::error!("audit write failed: {err}");
    }
}

/// A short audit summary of a successful call.
fn summarize_ok(tool: &str, value: &Value) -> String {
    let handle = value.get("handle").and_then(Value::as_str);
    match tool {
        "key_mint" => format!(
            "minted {} for {}",
            value.get("key_id").and_then(Value::as_str).unwrap_or("?"),
            value
                .get("principal")
                .and_then(Value::as_str)
                .unwrap_or("?")
        ),
        "key_revoke" => format!(
            "revoked {}",
            value.get("revoked").and_then(Value::as_str).unwrap_or("?")
        ),
        "grant_add" => format!(
            "granted {} on {}",
            value
                .get("principal")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            value
                .pointer("/grant/project_id")
                .and_then(Value::as_str)
                .unwrap_or("?")
        ),
        "grant_remove" => format!(
            "removed the grant of {} on {}",
            value
                .get("principal")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            value.get("removed").and_then(Value::as_str).unwrap_or("?")
        ),
        "create_project" => format!(
            "created {}",
            value.get("name").and_then(Value::as_str).unwrap_or("?")
        ),
        _ => handle.map_or_else(|| "ok".to_owned(), |handle| format!("{tool} {handle}")),
    }
}

fn cap_summary(summary: &str) -> String {
    if summary.len() <= 200 {
        return summary.to_owned();
    }
    let mut cut = 200;
    while !summary.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &summary[..cut])
}

// -------------------------------------------------- self-service minting

/// The grant level a self-service key deserves (wave 3 D3): operator when
/// today's rule holds at mint time — the key would be minted by the host's
/// own member acting as a role of kind operator on a hosted project —
/// else member. The level is stored metadata; the tools re-derive the rule
/// per call.
pub(crate) fn self_service_level(
    state: &AppState,
    project_id: &str,
    identity: &IdentityRecord,
    acting: Option<&str>,
) -> GrantLevel {
    let Ok(meta) = state.projects.load_meta(project_id) else {
        return GrantLevel::Member;
    };
    let Some(root) = meta.mailspace_root else {
        return GrantLevel::Member;
    };
    let requested = acting.unwrap_or(&state.hosted_default_identity);
    if super::live::operator_rule_holds(&root, identity, requested) {
        GrantLevel::Operator
    } else {
        GrantLevel::Member
    }
}
