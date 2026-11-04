//! The one place the bridge looks inside a message: a `tools/call` of a
//! tool that takes `project`, sent without one, gets the bridge's configured
//! project filled in. The wire to the server stays explicit; nothing is
//! inferred server-side.
//!
//! The tool list comes from the bridge's own `tools/list` request upstream,
//! made once per session on the first call that needs it; its reply is never
//! written to the model. Every other message is forwarded byte for byte.

use serde_json::{Value, json};

use super::client;
use super::{Outcome, Upstream, classify_reply};

/// Pages of `tools/list` followed before giving up (a cursor loop guard).
const MAX_TOOL_PAGES: usize = 100;

/// The id prefix of the bridge's own `tools/list` requests.
const LIST_ID: &str = "vivi-mcp-bridge-tools";

/// What to forward for one message, and a diagnostic to write if any.
pub(super) struct Forward {
    /// The body to POST: the original line, or the rewritten JSON.
    pub body: String,
    /// One diagnostic line (without the `vivi mcp: ` prefix).
    pub diag: Option<String>,
}

/// The project to fill in and the session's cached tool list.
pub(super) struct Injector {
    project: Option<String>,
    tools: Option<Vec<Value>>,
}

impl Injector {
    pub(super) fn new(project: Option<&str>) -> Self {
        Self {
            project: project.map(str::to_string),
            tools: None,
        }
    }

    /// The bytes to forward for `line`, the parsed `message` it holds. With
    /// no project, or when no call needs one, that is `line` itself.
    pub(super) fn apply(
        &mut self,
        upstream: &dyn Upstream,
        api_key: &str,
        line: &str,
        message: &Value,
    ) -> Forward {
        let unchanged = || Forward {
            body: line.to_string(),
            diag: None,
        };
        let Some(project) = self.project.clone() else {
            return unchanged();
        };
        let mut rewritten = message.clone();
        let filled = match &mut rewritten {
            Value::Array(items) => items.iter_mut().try_fold(false, |any, item| {
                self.fill(upstream, api_key, &project, item)
                    .map(|filled| any || filled)
            }),
            item => self.fill(upstream, api_key, &project, item),
        };
        match filled {
            Ok(true) => Forward {
                body: rewritten.to_string(),
                diag: None,
            },
            Ok(false) => unchanged(),
            Err(error) => Forward {
                body: line.to_string(),
                diag: Some(format!(
                    "cannot list the tools to fill the project ({error}); forwarding the call unchanged"
                )),
            },
        }
    }

    /// Add `project` to one `tools/call` that lacks it and whose tool takes
    /// it; whether the item changed.
    fn fill(
        &mut self,
        upstream: &dyn Upstream,
        api_key: &str,
        project: &str,
        item: &mut Value,
    ) -> Result<bool, String> {
        let Some(name) = lacks_project(item) else {
            return Ok(false);
        };
        let tools = self.tool_list(upstream, api_key)?;
        if !client::takes_project(tools, &name) {
            return Ok(false);
        }
        let arguments = item
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        item["params"]["arguments"] = client::with_project(&arguments, project);
        Ok(true)
    }

    /// The cached tool list, fetched on first use. A failure is not cached.
    fn tool_list(&mut self, upstream: &dyn Upstream, api_key: &str) -> Result<&[Value], String> {
        if self.tools.is_none() {
            self.tools = Some(fetch_tools(upstream, api_key)?);
        }
        Ok(self.tools.as_deref().unwrap_or_default())
    }
}

/// The tool name of a `tools/call` whose arguments carry no `project`.
fn lacks_project(item: &Value) -> Option<String> {
    if item.get("method").and_then(Value::as_str) != Some("tools/call") {
        return None;
    }
    let name = item.pointer("/params/name").and_then(Value::as_str)?;
    match item.pointer("/params/arguments") {
        None => {}
        Some(Value::Object(arguments)) if !arguments.contains_key("project") => {}
        Some(_) => return None,
    }
    Some(name.to_string())
}

/// Every tool the server lists, following `nextCursor` pages.
fn fetch_tools(upstream: &dyn Upstream, api_key: &str) -> Result<Vec<Value>, String> {
    let mut tools: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_TOOL_PAGES {
        let id = format!("{LIST_ID}-{page}");
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
        let result = list_page(upstream, api_key, &id, params)?;
        let listed = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or("tools/list result has no tools array")?;
        tools.extend(listed.iter().cloned());
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(tools)
}

/// One `tools/list` request: the `result` of the reply carrying `id`.
fn list_page(
    upstream: &dyn Upstream,
    api_key: &str,
    id: &str,
    params: Value,
) -> Result<Value, String> {
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list", "params": params });
    let response = upstream
        .post(api_key, body.to_string().as_bytes())
        .map_err(|error| error.to_string())?;
    let Outcome::Lines(lines) = classify_reply(response).map_err(|failure| failure.message)? else {
        return Err("the server sent an empty reply to tools/list".into());
    };
    let reply = lines
        .iter()
        .find_map(|line| match serde_json::from_str::<Value>(line) {
            Ok(reply) if reply.get("id") == Some(&json!(id)) => Some(reply),
            Ok(_) | Err(_) => None,
        })
        .ok_or("the server sent no JSON-RPC reply to tools/list")?;
    if let Some(message) = reply.pointer("/error/message").and_then(Value::as_str) {
        return Err(format!("tools/list refused: {message}"));
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| "the reply to tools/list has no result".into())
}

#[cfg(test)]
#[path = "inject_test.rs"]
mod tests;
