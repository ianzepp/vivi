//! The `/v1/mcp` route of the project-server fake: the same JSON-RPC
//! surface as the per-project `/mcp`, but project-scoped by an argument.
//! `show`, `thread`, `reply` and `done` take a required `project`; the rest
//! of the protocol is the parent's. Reaches the parent's private items
//! through `super::`.

use std::sync::Mutex;

use serde_json::{Value, json};

use super::{
    PROJECT, Reply, Request, State, authenticate, dispatch_one, empty_reply, error_reply,
    json_reply, parse_error_reply, record_mcp, sse_wrapped, tool_error, tools_list,
};

/// Tools that act on one project and so require the `project` argument.
const PROJECT_TOOLS: [&str; 4] = ["show", "thread", "reply", "done"];

/// `POST /v1/mcp`: record first, then refusal, then bearer auth, then
/// dispatch with the project rule. A body that is not valid JSON is a
/// `-32700` parse error, answered before any bearer judgment.
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
            return error_reply(401, "unauthorized", "the fake is refusing /mcp");
        }
    }
    let Some(caller) = authenticate(state, &bearer) else {
        return error_reply(
            401,
            "unauthorized",
            "the bearer is not a live token or API key",
        );
    };
    sse_wrapped(accept.as_deref(), dispatch(state, &caller, &parsed))
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

/// One message: `tools/list` gains the `project` property, a project tool
/// call is checked for its project, everything else is the parent's.
fn dispatch_item(state: &Mutex<State>, caller: &str, parsed: &Value) -> Option<Value> {
    let method = parsed
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method == "tools/list" {
        let id = parsed.get("id").cloned().unwrap_or(Value::Null);
        return Some(json!({ "jsonrpc": "2.0", "id": id, "result": scoped_tools_list() }));
    }
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

/// The tool-error text when the call names no project or a wrong one.
fn project_refusal(parsed: &Value) -> Option<String> {
    match parsed
        .pointer("/params/arguments/project")
        .and_then(Value::as_str)
    {
        None => Some("project is required".to_string()),
        Some(project) if project != PROJECT => Some(format!("unknown project: {project}")),
        Some(_) => None,
    }
}

/// The parent's tool list with a required `project` string on the project
/// tools.
fn scoped_tools_list() -> Value {
    let mut list = tools_list();
    if let Some(tools) = list.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
            if !PROJECT_TOOLS.contains(&name) {
                continue;
            }
            if let Some(schema) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) {
                if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
                    props.insert("project".to_string(), json!({ "type": "string" }));
                }
                schema.insert("required".to_string(), json!(["project"]));
            }
        }
    }
    list
}
