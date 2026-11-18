//! The Vivi record, as the workflows see it: status updates, parks with a
//! typed cause, and closes. `McpRecord` writes them to the vivi server
//! through `vivi mcp call`; `LogRecord` prints them; `MemoryRecord` keeps
//! them for the tests.

use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::config::{RecordConfig, expand};
use crate::error::SpikeError;
use crate::types::{ParkCause, RecordCall};

pub trait NeedRecord: Send + Sync {
    fn apply(&self, call: &RecordCall) -> Result<(), SpikeError>;
}

/// An in-memory record that keeps every call in order.
#[derive(Debug, Default)]
pub struct MemoryRecord {
    calls: Mutex<Vec<RecordCall>>,
}

impl MemoryRecord {
    pub fn calls(&self) -> Result<Vec<RecordCall>, SpikeError> {
        Ok(self.calls.lock()?.clone())
    }

    /// Calls for one need, in order.
    pub fn calls_for(&self, need: &str) -> Result<Vec<RecordCall>, SpikeError> {
        let calls = self.calls()?;
        Ok(calls.into_iter().filter(|c| call_need(c) == need).collect())
    }
}

impl NeedRecord for MemoryRecord {
    fn apply(&self, call: &RecordCall) -> Result<(), SpikeError> {
        self.calls.lock()?.push(call.clone());
        Ok(())
    }
}

fn call_need(call: &RecordCall) -> &str {
    match call {
        RecordCall::Status { need, .. }
        | RecordCall::Park { need, .. }
        | RecordCall::Close { need, .. } => need,
    }
}

/// Prints each call on the worker's standard output.
#[derive(Debug, Default)]
pub struct LogRecord;

impl NeedRecord for LogRecord {
    fn apply(&self, call: &RecordCall) -> Result<(), SpikeError> {
        println!("record {}", serde_json::to_string(call).unwrap_or_default());
        Ok(())
    }
}

/// Writes to the need's thread on the vivi server. The MCP API key is read
/// from `key_file` for each call and passed only in the child's
/// environment (`VIVI_MCP_API_KEY`), never in argv.
#[derive(Debug, Clone)]
pub struct McpRecord {
    pub config: RecordConfig,
}

impl NeedRecord for McpRecord {
    fn apply(&self, call: &RecordCall) -> Result<(), SpikeError> {
        let key_path = expand(&self.config.key_file);
        let key = std::fs::read_to_string(&key_path)
            .map_err(|e| SpikeError::Record(format!("{}: {e}", key_path.display())))?;
        for (tool, args) in mcp_calls(call, &self.config.project, &self.config.role) {
            self.call(&tool, &args, key.trim())?;
        }
        Ok(())
    }
}

impl McpRecord {
    fn call(&self, tool: &str, args: &Value, key: &str) -> Result<(), SpikeError> {
        let vivi = if self.config.vivi.is_empty() {
            "vivi".to_string()
        } else {
            expand(&self.config.vivi).display().to_string()
        };
        let out = Command::new(vivi)
            .args(["mcp", "call", "--server", &self.config.server, tool])
            .arg(args.to_string())
            .env("VIVI_MCP_API_KEY", key)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| SpikeError::Record(format!("vivi mcp call {tool}: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        Err(SpikeError::Record(format!(
            "vivi mcp call {tool} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// The MCP tool calls for one record call: a status is a reply on the
/// need; a park is a `parked` mark plus a reply with the typed cause; a
/// close is `done` with the summary as its note.
pub fn mcp_calls(call: &RecordCall, project: &str, role: &str) -> Vec<(String, Value)> {
    let with_role = |mut args: Value| {
        if !role.is_empty() {
            args["for"] = json!(role);
        }
        args
    };
    match call {
        RecordCall::Status { need, status } => vec![(
            "reply".to_string(),
            json!({"project": project, "handle": need, "body": format!("status: {status}")}),
        )],
        RecordCall::Park { need, slice, cause } => vec![
            (
                "mark".to_string(),
                with_role(json!({"project": project, "handle": need,
                    "marks": ["parked", format!("parked-{}", cause_class(cause))]})),
            ),
            (
                "reply".to_string(),
                json!({"project": project, "handle": need,
                    "body": format!("parked at slice {slice}: {}", cause_text(cause))}),
            ),
        ],
        RecordCall::Close { need, summary } => vec![(
            "done".to_string(),
            with_role(json!({"project": project, "handle": need, "note": summary})),
        )],
    }
}

fn cause_class(cause: &ParkCause) -> &'static str {
    match cause {
        ParkCause::WorkExhausted { .. } => "work",
        ParkCause::InfraExhausted { .. } => "infra",
        ParkCause::ProviderExhausted { .. } => "provider",
    }
}

fn cause_text(cause: &ParkCause) -> String {
    match cause {
        ParkCause::WorkExhausted {
            rounds,
            last_failure,
        } => format!("work exhausted after {rounds} rounds; last failure:\n{last_failure}"),
        ParkCause::InfraExhausted { step, detail } => {
            format!("infra exhausted at {step}: {detail}")
        }
        ParkCause::ProviderExhausted { step, waits } => {
            format!("provider still throttled at {step} after {waits} waits")
        }
    }
}

#[cfg(test)]
#[path = "record_test.rs"]
mod tests;
