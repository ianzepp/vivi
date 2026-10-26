//! The mind acts through the server's MCP tools, the way any MCP client
//! does: it holds a key, calls `pump_control`, `pump_action`, `pump_list`,
//! `pump_log` and `send`, and reads what they answer. It runs no `vivi`
//! command and writes no file in a project.
//!
//! [`McpActuator`] turns one permitted action into one or more tool calls.
//! The server decides what the key may do, so a refusal comes back as the
//! tool's error text, which the session sees on its next turn.

use std::path::Path;

use serde_json::{Value, json};

use super::actions::{ActionRequest, Actuator};
use super::authority::Action;
use crate::mcp::HttpUpstream;
use crate::mcp::client::{Client, result_is_error, result_text};

/// Longest tool answer kept in a receipt.
const MAX_RECEIPT: usize = 1500;

/// One tool call: name and arguments.
type Call = (&'static str, Value);

/// Calls one server tool and returns its text, or the failure text.
pub trait ToolCaller {
    /// # Errors
    /// Returns the transport failure, or the tool's own error text.
    fn call(&self, tool: &str, arguments: &Value) -> Result<String, String>;
}

/// The real caller: one blocking MCP conversation per call, with a bearer
/// key.
pub struct HttpCaller {
    server: String,
    key: String,
}

impl HttpCaller {
    /// A caller for `server` that presents the key in `key_file`.
    ///
    /// # Errors
    /// Fails when the key file cannot be read or is empty.
    pub fn from_key_file(server: &str, key_file: &Path) -> Result<Self, String> {
        let key = std::fs::read_to_string(key_file).map_err(|error| {
            format!("cannot read the mind's key {}: {error}", key_file.display())
        })?;
        let key = key.trim().to_string();
        if key.is_empty() {
            return Err(format!(
                "the mind's key file {} is empty",
                key_file.display()
            ));
        }
        Ok(Self {
            server: server.to_string(),
            key,
        })
    }
}

impl ToolCaller for HttpCaller {
    fn call(&self, tool: &str, arguments: &Value) -> Result<String, String> {
        // The client is blocking. The service runs a multi-thread runtime,
        // where `block_in_place` hands this thread's other work away; any
        // other runtime (a test) just blocks.
        blocking(|| {
            let upstream = HttpUpstream::new(&self.server);
            let mut client = Client::connect(&upstream, &self.key).map_err(|e| e.to_string())?;
            let result = client
                .call_tool(tool, arguments)
                .map_err(|e| e.to_string())?;
            let text = result_text(&result);
            if result_is_error(&result) {
                Err(text)
            } else {
                Ok(text)
            }
        })
    }
}

fn blocking<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Carries out permitted actions as MCP tool calls.
pub struct McpActuator<C> {
    caller: C,
}

impl<C: ToolCaller> McpActuator<C> {
    #[must_use]
    pub fn new(caller: C) -> Self {
        Self { caller }
    }
}

impl<C: ToolCaller> Actuator for McpActuator<C> {
    fn apply(&mut self, request: &ActionRequest) -> Result<String, String> {
        let mut receipt = Vec::new();
        for (tool, arguments) in calls_for(request)? {
            let answer = self
                .caller
                .call(tool, &arguments)
                .map_err(|error| format!("{tool}: {error}"))?;
            receipt.push(format!("{tool}: {}", clip(&answer)));
        }
        Ok(receipt.join("\n"))
    }

    fn feeds_back(&self) -> bool {
        true
    }
}

fn clip(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_RECEIPT {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_RECEIPT).collect();
    format!("{kept}\n[cut]")
}

/// The server's name for a project: its root's directory name, which is how
/// a hosted project is named.
fn project_name(request: &ActionRequest) -> Result<String, String> {
    request
        .project
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| String::from("the action names no project"))
}

fn need_of(request: &ActionRequest) -> Result<&str, String> {
    request
        .need
        .as_deref()
        .ok_or_else(|| String::from("the action names no need"))
}

fn step_of(request: &ActionRequest) -> Result<&str, String> {
    request
        .step
        .as_deref()
        .ok_or_else(|| String::from("the action names no step"))
}

/// The tool calls one action makes.
fn calls_for(request: &ActionRequest) -> Result<Vec<Call>, String> {
    let project = project_name(request)?;
    let note = request.note.as_deref();
    let calls = match request.kind {
        Action::PausePump => vec![control(&project, "pause")],
        Action::ResumePump => vec![control(&project, "resume")],
        Action::Requeue => vec![pump_action(
            &project,
            json!({ "action": "requeue", "steps": [step_of(request)?], "reason": note }),
        )],
        Action::Replan => vec![pump_action(
            &project,
            json!({ "action": "replan", "need": need_of(request)? }),
        )],
        Action::Answer => vec![pump_action(
            &project,
            json!({
                "action": "answer",
                "step": step_of(request)?,
                "word": request.word.as_deref().ok_or("the action names no word")?,
                "note": note,
            }),
        )],
        Action::Unlock => vec![pump_action(
            &project,
            json!({ "action": "unlock", "need": need_of(request)?, "note": note }),
        )],
        Action::ReadStatus => vec![
            (
                "pump_list",
                json!({ "project": project, "status": "open", "limit": 20 }),
            ),
            (
                "pump_log",
                json!({ "project": project, "file": "log", "lines": 40 }),
            ),
        ],
        Action::LetterToOperator | Action::Escalate => vec![letter(&project, request)],
        Action::FileFixNeed | Action::Upgrade | Action::SelfRelease => {
            return Err(format!(
                "{} is not wired to the server yet",
                request.kind.as_str()
            ));
        }
    };
    Ok(calls)
}

fn control(project: &str, action: &str) -> Call {
    (
        "pump_control",
        json!({ "project": project, "action": action }),
    )
}

/// `pump_action` arguments: the given object plus the project, with unset
/// (`null`) fields left out.
fn pump_action(project: &str, mut arguments: Value) -> Call {
    if let Some(object) = arguments.as_object_mut() {
        object.insert("project".into(), Value::String(project.to_string()));
        object.retain(|_, value| !value.is_null());
    }
    ("pump_action", arguments)
}

fn letter(project: &str, request: &ActionRequest) -> Call {
    let subject = match request.need.as_deref() {
        Some(need) => format!("mind: {} (need {need})", request.kind.as_str()),
        None => format!("mind: {}", request.kind.as_str()),
    };
    (
        "send",
        json!({
            "project": project,
            "kind": "mail",
            "to": "operator",
            "subject": subject,
            "body": request.note.as_deref().unwrap_or_default(),
        }),
    )
}

#[cfg(test)]
#[path = "mcp_actuator_test.rs"]
mod tests;
