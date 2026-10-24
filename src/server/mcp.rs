//! Streamable HTTP MCP for a project mailspace (phase-03 decided item 15).
//!
//! One endpoint per project: `POST /v1/projects/{id}/mcp`. The URL names the
//! project so a connector added by URL does not have to pass it, and the
//! Bearer (identity token or API key) is always the caller. Tools are a
//! curated, parameterized subset of the vivi command tree.

pub mod host;
pub mod letters;
mod live;
mod tools;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::keys::Caller;
use crate::projects_api::{authenticate_caller, project_error, require_existing_project};
use crate::rooms::error_response;

const PROTOCOL_LATEST: &str = "2025-03-26";
const SUPPORTED: &[&str] = &["2024-11-05", "2025-03-26", "2025-11-25"];

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
}

/// `POST /v1/projects/:id/mcp` — Streamable HTTP MCP (JSON-RPC 2.0).
pub async fn mcp_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(response) = require_existing_project(&state, &id).await {
        return response;
    }
    if let Err(response) = crate::projects_api::require_live_chat(&state, &id).await {
        return response;
    }
    let caller = match authenticate_caller(&state, &id, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    respond(
        &Surface::Project {
            state: &state,
            project_id: &id,
            caller: &caller,
        },
        &headers,
        body,
    )
    .await
}

/// Which endpoint a JSON-RPC body is for: one project's tools (the caller
/// already authenticated) or the host's.
enum Surface<'a> {
    Project {
        state: &'a AppState,
        project_id: &'a str,
        caller: &'a Caller,
    },
    Host {
        state: &'a AppState,
    },
}

/// Parse a JSON-RPC body (single or batch) and answer it for `surface`.
async fn respond(
    surface: &Surface<'_>,
    headers: &HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let bytes = match body {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "request body could not be read",
            );
        }
    };
    let sse = prefers_sse(headers);
    let parsed: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return rpc_http(
                StatusCode::BAD_REQUEST,
                jsonrpc_error(Value::Null, -32700, "parse error"),
                sse,
            );
        }
    };
    if let Some(items) = parsed.as_array() {
        let mut responses = Vec::new();
        for item in items {
            match handle_one(surface, item).await {
                Ok(Some(response)) => responses.push(response),
                Ok(None) => {}
                Err(response) => return response,
            }
        }
        return rpc_http(StatusCode::OK, Value::Array(responses), sse);
    }
    match handle_one(surface, &parsed).await {
        Ok(Some(value)) => rpc_http(StatusCode::OK, value, sse),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(response) => response,
    }
}

async fn handle_one(surface: &Surface<'_>, value: &Value) -> Result<Option<Value>, Response> {
    let req: JsonRpcRequest = match serde_json::from_value(value.clone()) {
        Ok(req) => req,
        Err(_) => {
            return Ok(Some(jsonrpc_error(Value::Null, -32600, "invalid request")));
        }
    };
    if req.jsonrpc.as_deref() != Some("2.0") {
        return Ok(Some(jsonrpc_error(
            req.id.clone().unwrap_or(Value::Null),
            -32600,
            "invalid request",
        )));
    }
    let Some(method) = req.method.as_deref() else {
        return Ok(Some(jsonrpc_error(
            req.id.clone().unwrap_or(Value::Null),
            -32600,
            "invalid request",
        )));
    };
    if method.starts_with("notifications/") {
        return Ok(None);
    }
    let id = req.id.clone().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => initialize(&req.params, surface),
        "ping" => json!({}),
        "tools/list" => match surface {
            Surface::Project { .. } => tools_list(),
            Surface::Host { .. } => host::tools_list(),
        },
        "tools/call" => {
            let name = req.params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = req.params.get("arguments").cloned().unwrap_or(json!({}));
            match surface {
                Surface::Project {
                    state,
                    project_id,
                    caller,
                } => call_tool(state, project_id, caller, name, arguments).await?,
                Surface::Host { state } => host::call_tool(state, name, arguments).await,
            }
        }
        _ => return Ok(Some(jsonrpc_error(id, -32601, "method not found"))),
    };
    Ok(Some(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })))
}

async fn call_tool(
    state: &AppState,
    project_id: &str,
    caller: &Caller,
    name: &str,
    arguments: Value,
) -> Result<Value, Response> {
    let store = state.projects.clone();
    let limiter = state.limiter.clone();
    let project_id = project_id.to_owned();
    let identity = caller.identity.clone();
    let acting_as = caller.acting_as.clone();
    // A key on a hosted project acts on the host's mailspace as a vivi
    // identity: the key's own, else the server's default.
    let hosted_as = caller.via_key.then(|| {
        acting_as
            .clone()
            .unwrap_or_else(|| state.hosted_default_identity.to_string())
    });
    let name = name.to_owned();
    match tokio::task::spawn_blocking(move || {
        if name == "pump" {
            return pump_tool(&store, &project_id, &arguments);
        }
        if let Some(run) = live::operator_tool(&name) {
            let Some(root) = live::hosted_root(&store, &project_id) else {
                return Err(tools::ToolError::new(format!(
                    "{name} is only available on a hosted project"
                )));
            };
            return run(
                &limiter,
                &project_id,
                &root,
                &identity,
                hosted_as.as_deref(),
                &arguments,
            );
        }
        if let Some(requested) = &hosted_as {
            if let Some(root) = live::hosted_root(&store, &project_id) {
                if live::handles(&name) {
                    return live::dispatch(
                        &limiter,
                        &project_id,
                        &root,
                        requested,
                        &name,
                        &arguments,
                    );
                }
            }
        }
        tools::dispatch(
            &store,
            &limiter,
            &project_id,
            &identity,
            acting_as.as_deref(),
            &name,
            &arguments,
        )
    })
    .await
    {
        Ok(Ok(value)) => Ok(tool_result(value, false)),
        Ok(Err(err)) => Ok(tool_result(json!({ "error": err.message }), true)),
        Err(err) => Err(project_error(crate::projects::ProjectError::Storage(
            format!("mcp tool task failed: {err}"),
        ))),
    }
}

/// `pump`: the hosted pump's health verdict, read-only, for any caller.
fn pump_tool(
    store: &crate::projects::ProjectStore,
    project_id: &str,
    arguments: &Value,
) -> Result<Value, tools::ToolError> {
    let seconds = |name: &str| -> Result<Option<u64>, tools::ToolError> {
        match arguments.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(Some)
                .ok_or_else(|| tools::ToolError::new(format!("{name}: expected whole seconds"))),
        }
    };
    let opts = vivi::pump::health::HealthOptions {
        since: seconds("since")?,
        stale_after: seconds("stale_after")?,
        now: None,
    };
    match crate::pump_health::read(store, project_id, &opts) {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(tools::ToolError::new(
            "pump health is only available on a hosted project".to_owned(),
        )),
        Err(message) => Err(tools::ToolError::new(message)),
    }
}

fn tool_result(value: Value, is_error: bool) -> Value {
    let text = match serde_json::to_string_pretty(&value) {
        Ok(text) => text,
        Err(_) => value.to_string(),
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn initialize(params: &Value, surface: &Surface<'_>) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_LATEST);
    let protocol = if SUPPORTED.contains(&requested) {
        requested
    } else {
        PROTOCOL_LATEST
    };
    let instructions = match surface {
        Surface::Project { .. } => PROJECT_INSTRUCTIONS,
        Surface::Host { .. } => host::INSTRUCTIONS,
    };
    json!({
        "protocolVersion": protocol,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "vivi-server",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": instructions,
    })
}

const PROJECT_INSTRUCTIONS: &str = "This is a vivi project mailspace. You are the authenticated member; never pass a from address. Start with overview, then list/show/thread to read, send/reply for new work, done/reopen for lifecycle, memo for durable notes, and graph for ready/blocked. On a hosted project, mark/unmark label a need or want, depend adds or drops prerequisites after the send, and board returns the project board.";

fn tools_list() -> Value {
    json!({
        "tools": [
            tool("overview", "Board/boot digest: who you are, open counts, ready handles, recent items.", json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            tool("list", "List needs, tasks, wants, mail, or memos. Defaults to open work items.", json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["need", "task", "want", "chat", "mail", "memo"] },
                    "status": { "type": "string", "enum": ["open", "done", "all"], "default": "open" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 50 },
                    "to": { "type": "string", "description": "Hosted projects: only letters addressed to this role or address" },
                    "from": { "type": "string", "description": "Hosted projects: only letters from this role or address" },
                    "marks": {
                        "description": "Hosted projects: only items carrying every one of these marks",
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" } }
                        ]
                    },
                    "for": { "type": "string", "description": "Hosted projects: list this role's folders instead of the acting identity's, or `all` for every role" }
                },
                "additionalProperties": false
            })),
            tool("show", "Show one letter or memo by 8-hex handle.", json!({
                "type": "object",
                "properties": { "handle": { "type": "string" } },
                "required": ["handle"],
                "additionalProperties": false
            })),
            tool("thread", "Show the reply thread around a handle.", json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 20 }
                },
                "required": ["handle"],
                "additionalProperties": false
            })),
            tool("send", "Send a need, task, want, chat, or mail as the authenticated caller. The server composes the letter and mints its id. For kind=chat, to is optional. For a need, task, or want the sugar arguments build the letter's front matter as the CLI's flags do (a need needs done_when, a want needs question; a missing one is refused with vivi's message). A body that opens with +++ is the whole letter, and a sugar argument beside it is refused. Mail and chat take no sugar arguments.", json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["need", "task", "want", "chat", "mail"] },
                    "to": {
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        ]
                    },
                    "subject": { "type": "string" },
                    "body": { "type": "string", "description": "The prose, or the whole letter when it opens with +++" },
                    "done_when": {
                        "description": "need (required there) or task: what must be true for the work to be done",
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        ]
                    },
                    "target": { "type": "string", "description": "need or task: the repo or checkout the work targets" },
                    "write_scope": {
                        "description": "need or task: paths the work may change",
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        ]
                    },
                    "serves": { "type": "string", "description": "need: the want this need serves" },
                    "explores": { "type": "string", "description": "need: the want this research need explores" },
                    "question": { "type": "string", "description": "want (required there): the question or goal the want explores" },
                    "depends_on": {
                        "description": "need, task, or want: handles of the items this one waits for; rides in the letter's front matter",
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" } }
                        ]
                    }
                },
                "required": ["kind", "to", "subject", "body"],
                "additionalProperties": false
            })),
            tool("reply", "Reply to a letter by handle as the authenticated caller.", json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string" },
                    "body": { "type": "string" }
                },
                "required": ["handle", "body"],
                "additionalProperties": false
            })),
            tool("done", "Close a need, task, or want. Travels as a stamped event letter when you authored it (same mailspace prefix); otherwise files a report reply. The newest event against an item controls its state, whichever member wrote it.", json!({
                "type": "object",
                "properties": {
                    "for": { "type": "string", "description": "Hosted projects: the role the command runs as, which must hold the item (the CLI's --for). Defaults to the acting identity." },
                    "handle": { "type": "string" },
                    "note": { "type": "string" }
                },
                "required": ["handle"],
                "additionalProperties": false
            })),
            tool("reopen", "Reopen a closed need, task, or want. Same authority rule as done. An absorbed item is final and is refused.", json!({
                "type": "object",
                "properties": {
                    "for": { "type": "string", "description": "Hosted projects: the role the command runs as, which must hold the item (the CLI's --for). Defaults to the acting identity." },
                    "handle": { "type": "string" },
                    "note": { "type": "string" }
                },
                "required": ["handle"],
                "additionalProperties": false
            })),
            tool("memo", "List, show, or save a durable memo on the server mailspace (memos do not sync to checkouts).", json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "show", "save"], "default": "list" },
                    "handle": { "type": "string" },
                    "subject": { "type": "string" },
                    "body": { "type": "string" }
                },
                "additionalProperties": false
            })),
            tool("graph", "Ready/blocked/done projection of work items from `depends_on` in the letter's front matter and from event letters. A legacy letter (no format header) contributes no dependencies.", json!({
                "type": "object",
                "properties": {
                    "view": { "type": "string", "enum": ["ready", "status"], "default": "status" }
                },
                "additionalProperties": false
            })),
            tool("mark", "Hosted projects only (API key). Add marks (labels such as `bug`) to a need or want held by the acting identity, as `vivi need mark` does. Marks are letters, numbers, dot, dash, or underscore; already-present marks change nothing. `for` names the holder role the command runs as, as `--for` does.", json!({
                "type": "object",
                "properties": {
                    "for": { "type": "string", "description": "Hosted projects: the role the command runs as, which must hold the item (the CLI's --for). Defaults to the acting identity." },
                    "handle": { "type": "string" },
                    "marks": {
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        ]
                    }
                },
                "required": ["handle", "marks"],
                "additionalProperties": false
            })),
            tool("unmark", "Hosted projects only (API key). Remove marks from a need or want held by the acting identity, as `vivi need unmark` does. `for` names the holder role the command runs as, as `--for` does.", json!({
                "type": "object",
                "properties": {
                    "for": { "type": "string", "description": "Hosted projects: the role the command runs as, which must hold the item (the CLI's --for). Defaults to the acting identity." },
                    "handle": { "type": "string" },
                    "marks": {
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        ]
                    }
                },
                "required": ["handle", "marks"],
                "additionalProperties": false
            })),
            tool("depend", "Hosted projects only (API key). Add and remove prerequisites of a need, task, or want after it was sent, as `vivi need|task|want depend --add --remove` does. Each changed dependency is a `dependency-added` / `dependency-removed` letter on the item, so the change reaches every member; per dependency the newest letter wins, over a dependency named at send too. Only the item's author, a mind, or an operator may change it; any other role is refused. Every handle resolves before anything is written. Returns the changes and the item's prerequisites as they stand. `for` names the role the command runs as, as `--for` does.", json!({
                "type": "object",
                "properties": {
                    "for": { "type": "string", "description": "Hosted projects: the role the command runs as, which must hold the item (the CLI's --for). Defaults to the acting identity." },
                    "handle": { "type": "string", "description": "The need, task, or want to change" },
                    "add": {
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        ],
                        "description": "Handles of items to add as prerequisites"
                    },
                    "remove": {
                        "oneOf": [
                            { "type": "string" },
                            { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        ],
                        "description": "Handles of prerequisites to remove, including ones named at send"
                    }
                },
                "required": ["handle"],
                "additionalProperties": false
            })),
            tool("pump", "Hosted projects only. Read-only pump health: verdict ok/degraded/stalled/unknown with reasons, control and writer state, last tick, recent exceptions. Pass a previous `as_of` as `since` for `changed_since`.", json!({
                "type": "object",
                "properties": {
                    "since": { "type": "integer", "minimum": 0, "description": "Epoch seconds, a previous answer's as_of" },
                    "stale_after": { "type": "integer", "minimum": 1, "description": "Seconds without a tick before a running pump is stalled (default 30 ticks, at least 600)" }
                },
                "additionalProperties": false
            })),
            tool("pump_control", "Hosted projects only, operator only. Pause, resume, stop, or start the project's pump, as `vivi pump pause|resume|stop|start` do (`start` is `resume`: both set the mode to running). Needs the operator's API key: minted by the host's own member and acting as a role of kind operator; a member token or any other key is refused. Returns the resulting control state.", json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["pause", "resume", "stop", "start"] }
                },
                "required": ["action"],
                "additionalProperties": false
            })),
            tool("pump_rungs", "Hosted projects only, operator only. Show or set the pump's rung commands (`[pump.rungs]` in the project's mailspace config, or `[pump.targets.<id>.rungs]` with `target`), as `vivi pump rungs show|set` do. Same authorization as pump_control: the operator's API key, minted by the host's own member and acting as a role of kind operator; show is held to it too. `set` needs a `rung` (task, need, main or release) and a non-blank `command`, is refused for an unknown rung or target, and is rate limited as a write. Show ignores `rung` and `command`; with `target` it answers that target's effective table (its own rung command, else the workspace one). Pools, timeout, roles and other config are not exposed for change, and a set is refused while the rung table has no effective rung pools (set `pools` first). Returns {target, task, need, main, release, timeout, pools} as the table stands afterwards, null for an unset command.", json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["show", "set"] },
                    "rung": { "type": "string", "enum": ["task", "need", "main", "release"], "description": "The rung to set; required for set" },
                    "command": { "type": "string", "description": "The shell command that proves the rung; required for set, must not be blank" },
                    "target": { "type": "string", "description": "A pump target id; omit for the workspace-level rungs" }
                },
                "required": ["action"],
                "additionalProperties": false
            })),
            tool("pool_bands", "Hosted projects only, operator only. Show, set, or unset one pool's band table (`[pools.<pool>.bands]` in the project's mailspace config), as `vivi pool bands set|unset` and `vivi pool show` do: which model and reasoning effort serves each band P0 to P5. Same authorization as pump_control: the operator's API key, minted by the host's own member and acting as a role of kind operator; show is held to it too. `set` replaces that band's entry with one {model, reasoning} choice (reasoning defaults to xhigh) and is refused for a model not in the pool's `harness.models` (the error names the allowed list), an unknown pool (the error names the pools) or a malformed band; `unset` removes the band's entry. Both are rate limited as writes. Only the bands table of the one named pool can change: no other pool, role, remote, harness or config key is reachable. Returns {pool, bands: {band: [{model, reasoning}]}, harness_models} as the table stands afterwards.", json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["show", "set", "unset"] },
                    "pool": { "type": "string", "description": "Pool name" },
                    "band": { "type": "string", "enum": ["P0", "P1", "P2", "P3", "P4", "P5"], "description": "Band; required for set and unset, ignored by show" },
                    "model": { "type": "string", "description": "Model slug, one of the pool's harness.models; required for set" },
                    "reasoning": { "type": "string", "description": "Reasoning effort, e.g. low or xhigh; optional for set (default xhigh)" }
                },
                "required": ["action", "pool"],
                "additionalProperties": false
            })),
            tool("pump_send", "Hosted projects only, operator only. Hand a need to the pump: file one open pump record for it, as `vivi pump send --need <handle>` does; the pump reads the record on its next tick. Same authorization as pump_control: the operator's API key, minted by the host's own member and acting as a role of kind operator. Refused for an unknown handle, anything that is not a need, a need not addressed to the pump identity, or a need that already has an open record (the error names it). Rate limited as a write. Returns the record.", json!({
                "type": "object",
                "properties": {
                    "need": { "type": "string", "description": "The need handle the dispatch covers" },
                    "body": { "type": "string", "description": "Record body, literal text (the CLI's - and @path forms are not accepted). Defaults to a dispatch line." },
                    "from": { "type": "string", "description": "Sender identity (the CLI's --from); defaults to the project's [pump] from" }
                },
                "required": ["need"],
                "additionalProperties": false
            })),
            tool("pump_list", "Hosted projects only, operator only. List pump records, oldest first, as `vivi pump list` does. Same authorization as pump_control. At most `limit` records are returned (the newest when more match); `total` counts every match. Returns {records, total, truncated}.", json!({
                "type": "object",
                "properties": {
                    "need": { "type": "string", "description": "Only records of this need" },
                    "status": { "type": "string", "enum": ["open", "done", "all"], "default": "open" },
                    "limit": { "type": "integer", "minimum": 1, "default": 50, "description": "Records returned; at most 100" }
                },
                "additionalProperties": false
            })),
            tool("pump_show", "Hosted projects only, operator only. Show one pump record and its body, as `vivi pump show` does. Same authorization as pump_control. Returns the record (handle, need, status, from, to, date, subject, outcome when done) and its body.", json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Pump record handle" }
                },
                "required": ["handle"],
                "additionalProperties": false
            })),
            tool("board", "Hosted projects only (API key). The project board as `vivi board --json`, bounded: open tasks, needs and wants per role (newest first, capped, with `*_hidden` counts), each last event's note cut to `note_preview`, each want's `serving` needs with their state, totals, and registered goals. Optional graph view. `full` lifts every bound.", json!({
                "type": "object",
                "properties": {
                    "for": { "type": "string", "description": "Only this role's board" },
                    "wants_cap": { "type": "integer", "minimum": 1, "default": 5, "description": "Wants shown per role; the rest are counted as hidden" },
                    "tasks_cap": { "type": "integer", "minimum": 1, "default": 5, "description": "Tasks shown per role; the rest are counted in tasks_hidden" },
                    "needs_cap": { "type": "integer", "minimum": 1, "default": 5, "description": "Needs shown per role; the rest are counted in needs_hidden" },
                    "graph_cap": { "type": "integer", "minimum": 1, "default": 25, "description": "With graph: nodes listed in each of ready, blocked and active; the full counts are ready_total, blocked_total and active_total" },
                    "notes": { "type": "boolean", "default": false, "description": "Keep each last event's whole note instead of a short preview" },
                    "full": { "type": "boolean", "default": false, "description": "No caps, whole notes and the graph's node table; an explicit cap still applies" },
                    "graph": { "type": "boolean", "default": false }
                },
                "additionalProperties": false
            })),
        ]
    })
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema,
    })
}

fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn prefers_sse(headers: &HeaderMap) -> bool {
    let accept = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    accept.contains("text/event-stream") && !accept.contains("application/json")
}

fn rpc_http(status: StatusCode, body: Value, sse: bool) -> Response {
    if sse {
        let data = body.to_string();
        (
            status,
            [
                (axum::http::header::CONTENT_TYPE, "text/event-stream"),
                (axum::http::header::CACHE_CONTROL, "no-store"),
            ],
            format!("event: message\ndata: {data}\n\n"),
        )
            .into_response()
    } else {
        (status, Json(body)).into_response()
    }
}
