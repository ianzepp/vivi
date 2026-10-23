//! One-shot MCP client over the bridge's own transport: `vivi mcp tools`
//! and `vivi mcp call`.
//!
//! A [`Client`] reuses the bridge [`Session`]: the same API key bearer and
//! the same JSON and SSE reply handling. On top of it
//! sits the little MCP handshake (`initialize`, then the `initialized`
//! notification) and a typed request: the `result` of the reply, or a
//! [`ClientError`] naming the transport failure or the JSON-RPC error.
//!
//! Why it exists: an MCP client such as Claude Code caches the tool list at
//! session start, so a tool added by a server upgrade stays invisible until
//! a restart. Any shell agent can reach it through these two commands.

use std::fmt;
use std::io::{self, Sink};

use serde_json::{Value, json};

use super::{Outcome, Session, Upstream};

/// The protocol revision offered in `initialize`; the server may answer
/// with an earlier one it supports.
const PROTOCOL_VERSION: &str = "2025-03-26";

/// Pages of `tools/list` followed before giving up (a cursor loop guard).
const MAX_TOOL_PAGES: usize = 100;

/// Why a one-shot request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// No usable answer: unreachable, refused (HTTP status in the text),
    /// authentication failed, or a reply that is not a JSON-RPC response.
    Transport(String),
    /// The server answered with a JSON-RPC `error`.
    Rpc {
        /// The JSON-RPC error code.
        code: i64,
        /// The JSON-RPC error message.
        message: String,
    },
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => f.write_str(message),
            Self::Rpc { code, message } => write!(f, "JSON-RPC error {code}: {message}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// One MCP conversation: a bridge session whose output is discarded and a
/// request counter.
pub struct Client<'a> {
    session: Session<'a, Sink, Sink>,
    next_id: u64,
}

impl<'a> Client<'a> {
    /// Open a conversation and run the handshake.
    ///
    /// # Errors
    /// Returns the failure of `initialize` or of the `initialized`
    /// notification.
    pub fn connect(upstream: &'a dyn Upstream, api_key: &'a str) -> Result<Self, ClientError> {
        let mut client = Self {
            session: Session::new(upstream, api_key, io::sink(), io::sink()),
            next_id: 1,
        };
        client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "vivi-mcp", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        client.notify("notifications/initialized")?;
        Ok(client)
    }

    /// Every tool the server lists, following `nextCursor` pages.
    ///
    /// # Errors
    /// Returns the failure of a `tools/list` request, or a transport error
    /// for a result with no `tools` array.
    pub fn tools(&mut self) -> Result<Vec<Value>, ClientError> {
        let mut tools: Vec<Value> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let result = self.request("tools/list", params)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    ClientError::Transport("tools/list result has no tools array".into())
                })?;
            tools.extend(page.iter().cloned());
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

    /// Call one tool and return the `result` object as the server sent it
    /// (a tool failure is a result with `isError: true`, not an error).
    ///
    /// # Errors
    /// Returns the failure of the `tools/call` request.
    pub fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<Value, ClientError> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }

    /// One request: the reply's `result`, or the error it carried.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let outcome = self
            .session
            .serve(body.to_string().as_bytes())
            .map_err(|failure| ClientError::Transport(failure.message))?;
        let Outcome::Lines(lines) = outcome else {
            return Err(ClientError::Transport(format!(
                "the server sent an empty reply to {method}"
            )));
        };
        let reply = lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|reply| reply.get("id") == Some(&json!(id)))
            .ok_or_else(|| {
                ClientError::Transport(format!("the server sent no JSON-RPC reply to {method}"))
            })?;
        if let Some(error) = reply.get("error") {
            return Err(ClientError::Rpc {
                code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| ClientError::Transport(format!("the reply to {method} has no result")))
    }

    /// One notification: no reply is expected, a refusal is an error.
    fn notify(&mut self, method: &str) -> Result<(), ClientError> {
        let body = json!({ "jsonrpc": "2.0", "method": method });
        self.session
            .serve(body.to_string().as_bytes())
            .map(|_| ())
            .map_err(|failure| ClientError::Transport(failure.message))
    }
}

/// One output line per tool: its name, then the first line of its
/// description (the name alone when it has none).
#[must_use]
pub fn tool_lines(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .map(|tool| {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or("?");
            let summary = tool
                .get("description")
                .and_then(Value::as_str)
                .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
                .unwrap_or_default();
            if summary.is_empty() {
                name.to_string()
            } else {
                format!("{name}\t{summary}")
            }
        })
        .collect()
}

/// The text of a tool result: every `content[].text` block, joined by
/// newlines.
#[must_use]
pub fn result_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Whether the tool called `name` in a `tools/list` array declares a
/// `project` input property.
#[must_use]
pub fn takes_project(tools: &[Value], name: &str) -> bool {
    tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|tool| tool.get("inputSchema"))
        .and_then(|schema| schema.get("properties"))
        .and_then(|properties| properties.get("project"))
        .is_some()
}

/// The arguments with `"project": project` added when they carry no
/// `project` key. An explicit `project` is kept; a non-object comes back
/// unchanged.
#[must_use]
pub fn with_project(arguments: &Value, project: &str) -> Value {
    let mut arguments = arguments.clone();
    if let Some(object) = arguments.as_object_mut() {
        object
            .entry("project")
            .or_insert_with(|| Value::String(project.to_string()));
    }
    arguments
}

/// Whether a tool result reports a tool failure.
#[must_use]
pub fn result_is_error(result: &Value) -> bool {
    result.get("isError").and_then(Value::as_bool) == Some(true)
}

/// Parse tool arguments: JSON text that must be an object.
///
/// # Errors
/// Returns a message when the text is not JSON or not a JSON object.
pub fn parse_arguments(text: &str) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("arguments are not JSON: {error}"))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err("arguments must be a JSON object".into())
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
