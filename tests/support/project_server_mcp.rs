//! The `/v1/mcp` route of the project-server fake: JSON-RPC 2.0 over an API
//! key, project-scoped by an argument. `show`, `thread`, `reply` and `done`
//! take a required `project`. Reaches the parent's private items through
//! `super::`.

use std::sync::Mutex;

use serde_json::{Value, json};

use super::{
    Item, McpSeen, PROJECT, Reply, ReplyRecord, Report, Request, State, empty_reply, error_reply,
    json_reply, token_member,
};

/// MCP protocol versions the fake echoes back verbatim.
const SUPPORTED_PROTOCOLS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-11-25"];
/// MCP protocol version handed back when the client's is unknown.
const FALLBACK_PROTOCOL: &str = "2025-03-26";
/// Tools that act on one project and so require the `project` argument.
const PROJECT_TOOLS: [&str; 4] = ["show", "thread", "reply", "done"];

/// `POST /v1/mcp`: record first, then refusal, then bearer auth (API keys
/// only), then dispatch with the project rule. A body that is not valid JSON
/// is a `-32700` parse error, answered before any bearer judgment.
pub(super) fn handle(state: &Mutex<State>, request: &Request) -> Reply {
    let bearer = request.bearer().unwrap_or_default().to_string();
    let accept = request.header("accept").map(str::to_string);
    let parsed = match serde_json::from_slice::<Value>(&request.body) {
        Ok(parsed) => parsed,
        Err(_) => {
            record_mcp(state, &request.path, &bearer, Value::Null, &accept);
            return parse_error_reply();
        }
    };
    record_mcp(state, &request.path, &bearer, parsed.clone(), &accept);
    {
        let mut state = state.lock().unwrap();
        if state.refuse_always || state.refuse_next > 0 {
            state.refuse_next = state.refuse_next.saturating_sub(1);
            return error_reply(401, "unauthorized", "the fake is refusing /v1/mcp");
        }
    }
    let caller = match authenticate(state, &bearer) {
        Ok(caller) => caller,
        Err(refusal) => return refusal,
    };
    sse_wrapped(accept.as_deref(), dispatch(state, &caller, &parsed))
}

/// Record one `/v1/mcp` request as it arrived, before any judgment.
fn record_mcp(
    state: &Mutex<State>,
    path: &str,
    bearer: &str,
    body: Value,
    accept: &Option<String>,
) {
    state.lock().unwrap().mcp_seen.push(McpSeen {
        path: path.to_string(),
        bearer: bearer.to_string(),
        body,
        accept: accept.clone(),
    });
}

/// The one parse-error reply for a body that is not valid JSON.
fn parse_error_reply() -> Reply {
    json_reply(
        400,
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32700, "message": "parse error" },
        }),
    )
}

/// Wrap a non-empty body in one SSE frame when the client asked for it.
fn sse_wrapped(accept: Option<&str>, reply: Reply) -> Reply {
    if accept == Some("text/event-stream") && !reply.body.is_empty() {
        Reply {
            status: reply.status,
            content_type: "text/event-stream",
            body: format!("event: message\ndata: {}\n\n", reply.body),
        }
    } else {
        reply
    }
}

/// A batch is answered by the array of the individual replies, notifications
/// omitted; a lone notification is 202 with an empty body.
fn dispatch(state: &Mutex<State>, caller: &str, parsed: &Value) -> Reply {
    if let Value::Array(items) = parsed {
        let replies: Vec<Value> = items
            .iter()
            .filter_map(|item| dispatch_item(state, caller, item))
            .collect();
        if replies.is_empty() {
            return empty_reply(202);
        }
        return json_reply(200, Value::Array(replies));
    }
    dispatch_item(state, caller, parsed)
        .map_or_else(|| empty_reply(202), |reply| json_reply(200, reply))
}

/// One message: a project tool call is checked for its project, everything
/// else goes straight to `dispatch_one`.
fn dispatch_item(state: &Mutex<State>, caller: &str, parsed: &Value) -> Option<Value> {
    let method = parsed
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method == "tools/call" {
        let called = parsed
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if PROJECT_TOOLS.contains(&called)
            && let Some(refusal) = project_refusal(parsed)
        {
            let id = parsed.get("id").cloned().unwrap_or(Value::Null);
            return Some(json!({ "jsonrpc": "2.0", "id": id, "result": tool_error(refusal) }));
        }
    }
    dispatch_one(state, caller, parsed)
}

/// The tool-error text when the call names no project or one this key has no
/// grant on. An unknown project and an ungranted one share one text, so a
/// key cannot probe which projects exist.
fn project_refusal(parsed: &Value) -> Option<String> {
    match parsed
        .pointer("/params/arguments/project")
        .and_then(Value::as_str)
    {
        None => Some("project is required".to_string()),
        Some(project) if project != PROJECT => Some(format!("this key has no grant on {project}")),
        Some(_) => None,
    }
}

/// One JSON-RPC message's reply value; `None` for a notification.
fn dispatch_one(state: &Mutex<State>, caller: &str, parsed: &Value) -> Option<Value> {
    let method = parsed
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method.starts_with("notifications/") {
        return None;
    }
    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
    let called = parsed.pointer("/params/name").and_then(Value::as_str);
    if method == "tools/call" && called == Some("rpc_error") {
        return Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32602, "message": "invalid params from the fake" },
        }));
    }
    let result = match method {
        "initialize" => initialize_result(parsed),
        "ping" => json!({}),
        "tools/list" => tools_list(),
        "tools/call" => call_tool(state, caller, parsed),
        other => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") },
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// `initialize`: echo a supported protocol version, else the fallback.
fn initialize_result(parsed: &Value) -> Value {
    let requested = parsed
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let version = if SUPPORTED_PROTOCOLS.contains(&requested) {
        requested
    } else {
        FALLBACK_PROTOCOL
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "fake-vivi-server", "version": "0" },
    })
}

/// The tool list: a required `project` string on the project tools.
fn tools_list() -> Value {
    let tool = |name: &str, description: &str, mut properties: Value| {
        let mut schema = json!({ "type": "object" });
        if PROJECT_TOOLS.contains(&name) {
            properties["project"] = json!({ "type": "string" });
            schema["required"] = json!(["project"]);
        }
        schema["properties"] = properties;
        json!({ "name": name, "description": description, "inputSchema": schema })
    };
    json!({
        "tools": [
            tool("echo", "echo", json!({ "text": { "type": "string" } })),
            tool("show", "show one item", json!({ "handle": { "type": "string" } })),
            tool(
                "thread",
                "show one item and its replies",
                json!({ "handle": { "type": "string" } }),
            ),
            tool(
                "reply",
                "append a reply to one item",
                json!({ "handle": { "type": "string" }, "body": { "type": "string" } }),
            ),
            tool("done", "mark one item done", json!({ "handle": { "type": "string" } })),
        ],
    })
}

/// A successful tool result carrying one text block.
fn tool_text(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

/// A failed tool result: still HTTP 200, `isError` true.
fn tool_error(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

/// `tools/call`: `echo` reflects `<caller>: <text>`; the item tools read
/// and record against the seeded items, with the caller; unknown tools are
/// a tool-level error, still HTTP 200.
fn call_tool(state: &Mutex<State>, caller: &str, parsed: &Value) -> Value {
    let name = parsed
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut state = state.lock().unwrap();
    match name {
        "echo" => {
            let text = parsed
                .pointer("/params/arguments/text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            tool_text(format!("{caller}: {text}"))
        }
        "show" | "thread" | "reply" | "done" => item_tool(&mut state, caller, name, parsed),
        other => tool_error(format!("unknown tool: {other}")),
    }
}

/// The item tools. `show` renders the item, `thread` the item and its
/// replies, `reply` appends a reply, `done` marks the item done; a reply
/// and a done are recorded as reports with the caller. A missing handle or
/// an unknown one is a tool-level error.
fn item_tool(state: &mut State, caller: &str, name: &str, parsed: &Value) -> Value {
    let Some(handle) = parsed
        .pointer("/params/arguments/handle")
        .and_then(Value::as_str)
    else {
        return tool_error(format!("{name} needs a handle"));
    };
    let Some(index) = state.items.iter().position(|item| item.handle == handle) else {
        return tool_error(format!("unknown item: {handle}"));
    };
    match name {
        "show" => tool_text(item_text(&state.items[index], false)),
        "thread" => tool_text(item_text(&state.items[index], true)),
        "reply" => {
            let body = parsed
                .pointer("/params/arguments/body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            state.items[index].replies.push(ReplyRecord {
                by: caller.to_string(),
                body: body.clone(),
            });
            state.reports.push(Report {
                kind: "reply",
                item: handle.to_string(),
                by: caller.to_string(),
                body,
            });
            tool_text(format!("reply recorded on {handle} by {caller}"))
        }
        "done" => {
            state.items[index].state = "done".to_string();
            state.reports.push(Report {
                kind: "done",
                item: handle.to_string(),
                by: caller.to_string(),
                body: String::new(),
            });
            tool_text(format!("done {handle} by {caller}"))
        }
        _ => tool_error(format!("unknown item tool: {name}")),
    }
}

/// One item as the `show`/`thread` text: handle, subject, body and state,
/// and, for `thread`, the replies in order.
fn item_text(item: &Item, with_replies: bool) -> String {
    let mut text = format!(
        "handle: {}\nsubject: {}\nbody: {}\nstate: {}",
        item.handle, item.subject, item.body, item.state
    );
    if with_replies {
        text.push_str("\nreplies:");
        if item.replies.is_empty() {
            text.push_str(" none");
        }
        for reply in &item.replies {
            text.push_str(&format!("\n- {}: {}", reply.by, reply.body));
        }
    }
    text
}

/// The member behind a live API key. A live identity token is refused with
/// its own text; any other bearer is the generic 401.
fn authenticate(state: &Mutex<State>, bearer: &str) -> Result<String, Reply> {
    let member = state
        .lock()
        .unwrap()
        .keys
        .iter()
        .find(|key| key.secret == bearer && !key.revoked)
        .map(|key| key.created_by.clone());
    if let Some(member) = member {
        return Ok(member);
    }
    if token_member(state, bearer).is_some() {
        return Err(error_reply(
            401,
            "unauthorized",
            "identity tokens are not accepted on /v1/mcp",
        ));
    }
    Err(error_reply(
        401,
        "unauthorized",
        "the bearer is not a live API key",
    ))
}
