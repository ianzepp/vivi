//! A hosted project's optional upstream: `[origin]` in its `mailspace.toml`.
//!
//! ```toml
//! [origin]
//! server = "http://127.0.0.1:18450"
//! project = "up"
//! ```
//!
//! Every item belongs to exactly one server and operations go to the owner;
//! nothing is copied. A handle this project does not hold is forwarded to the
//! origin as the same tool call, signed in with this server's machine key.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::mcp::HttpUpstream;
use crate::mcp::client::{self, Client};
use crate::server::mcp::tools::ToolError;

/// Origin forwards a request may carry before the chain is refused.
const MAX_HOPS: u32 = 4;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Origin {
    pub(super) server: String,
    pub(super) project: String,
}

#[derive(Deserialize)]
struct ConfigFile {
    origin: Option<Origin>,
}

/// The `[origin]` table of the mailspace at `root`, if any.
pub(super) fn config(root: &Path) -> Result<Option<Origin>, ToolError> {
    let path = root.join(".vivi").join("mailspace.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| ToolError::new(format!("{}: {e}", path.display())))?;
    let parsed: ConfigFile = toml::from_str(&text)
        .map_err(|e| ToolError::new(format!("{}: [origin]: {e}", path.display())))?;
    Ok(parsed.origin)
}

/// How a `list` or `board` call reaches the origin.
pub(super) enum Reach {
    Local,
    Origin,
    All,
}

/// Read and remove the `origin` argument of a `list` or `board` call.
pub(super) fn take_reach(arguments: &mut Value) -> Result<Reach, ToolError> {
    let Some(object) = arguments.as_object_mut() else {
        return Ok(Reach::Local);
    };
    match object.remove("origin") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(Reach::Local),
        Some(Value::Bool(true)) => Ok(Reach::Origin),
        Some(Value::String(s)) if s == "origin" => Ok(Reach::Origin),
        Some(Value::String(s)) if s == "all" => Ok(Reach::All),
        Some(_) => Err(ToolError::new(
            "origin must be true, false, \"origin\" or \"all\"",
        )),
    }
}

/// Forward `tool` to the origin with the same arguments, `project` replaced.
/// A JSON-object answer gains `"origin": <server url>`.
pub(super) fn forward(
    origin: &Origin,
    tool: &str,
    arguments: &Value,
    hops: u32,
) -> Result<Value, ToolError> {
    if hops >= MAX_HOPS {
        return Err(ToolError::new("origin chain too long (loop?)"));
    }
    let server = origin.server.trim_end_matches('/').to_owned();
    let mut args = arguments.clone();
    if let Some(object) = args.as_object_mut() {
        object.insert("project".into(), json!(origin.project));
    }
    let attempt = |bearer: &str| -> Result<Value, String> {
        let upstream = HttpUpstream::new(&server).with_hops(hops + 1);
        let mut mcp = Client::connect(&upstream, bearer).map_err(|e| e.to_string())?;
        mcp.call_tool(tool, &args).map_err(|e| e.to_string())
    };
    let fail = |e: String| ToolError::new(format!("origin {server} {tool}: {e}"));
    let bearer = crate::machine_key::sign_in(&server, false)
        .map_err(|e| fail(format!("machine sign-in: {e}")))?;
    let result = match attempt(&bearer) {
        // A session the origin no longer knows (it restarted): sign in again, once.
        Err(e) if crate::machine_key::is_refused_bearer(&e) => {
            crate::machine_key::forget(&server, false).map_err(|e| fail(e.to_string()))?;
            let bearer = crate::machine_key::sign_in(&server, false)
                .map_err(|e| fail(format!("machine sign-in: {e}")))?;
            attempt(&bearer)
        }
        other => other,
    }
    .map_err(fail)?;
    let text = client::result_text(&result);
    let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "text": text }));
    if client::result_is_error(&result) {
        let message = parsed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or(&text)
            .to_owned();
        return Err(fail(message));
    }
    let mut parsed = parsed;
    if let Some(object) = parsed.as_object_mut() {
        object.insert("origin".into(), json!(server));
    }
    Ok(parsed)
}

/// Merge a local and an origin `list` answer: `items` of both, each tagged
/// `from` (the item's own sender is kept as `sender`). The origin side may be an error (unreachable), reported beside the
/// local items rather than hiding them.
pub(super) fn merge_list(local: Value, remote: Result<Value, ToolError>) -> Value {
    let mut out = local;
    let tag = |items: &mut Vec<Value>, from: &str, source: &Value| {
        for item in source
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let mut item = item.clone();
            if let Some(object) = item.as_object_mut() {
                // `source` says which server owns the item; `from` stays the
                // item's sender, as in every other list.
                object.insert("source".into(), json!(from));
            }
            items.push(item);
        }
    };
    let mut items = Vec::new();
    tag(&mut items, "local", &out);
    match remote {
        Ok(remote) => {
            tag(&mut items, "origin", &remote);
            if let (Some(object), Some(url)) = (out.as_object_mut(), remote.get("origin")) {
                object.insert("origin".into(), url.clone());
            }
        }
        Err(error) => {
            if let Some(object) = out.as_object_mut() {
                object.insert("origin_error".into(), json!(error.message));
            }
        }
    }
    if let Some(object) = out.as_object_mut() {
        object.insert("items".into(), Value::Array(items));
    }
    out
}

/// A `board` answer from both sides, kept apart (a board is not a flat list).
pub(super) fn merge_board(local: Value, remote: Result<Value, ToolError>) -> Value {
    match remote {
        Ok(remote) => json!({ "local": local, "origin": remote }),
        Err(error) => json!({ "local": local, "origin_error": error.message }),
    }
}
