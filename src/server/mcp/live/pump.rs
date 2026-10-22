//! `pump_control` and `pump_gate`: operator-only pump tools for a hosted
//! project.
//!
//! The same library call the CLI's `vivi pump pause|resume|stop|start`
//! makes ([`vivi::pump::control::set_project_mode`]); the control state
//! lives in the host's `pump.db`, not in the server. In the CLI `start` is
//! `resume`: both set the mode to running.
//!
//! `pump_gate` shows or sets the pump gate command (`[pump.gate] command` in
//! the project's `.vivi/mailspace.toml`) through
//! [`vivi::pump::gate_config`]. It is the only config the server edits:
//! pools, roles and free-form config stay with the host, because a member who
//! could repoint a pool's `remote_url` could send code elsewhere.
//!
//! The caller of either tool must be the operator:
//!
//! 1. an API key (a member token never reaches a hosted mailspace),
//! 2. minted by the host's own member (the creator registered for the
//!    mailspace's own sync key, as `vivi sync api-key mint` does), so a key
//!    another member minted for itself cannot steer the pump, and
//! 3. acting as a role of kind `operator`.

use std::path::Path;

use serde_json::{Value, json};
use vivi::pump::control::{PumpMode, inspect, project_home, set_project_mode};
use vivi::pump::gate_config;

use crate::identities::IdentityRecord;
use crate::mcp::tools::{ToolError, rate_limit, req_str};
use crate::safety::RateLimiter;

use super::{Hosted, lib_error};

/// Run `pump_control` for `caller` (the key's minting member) acting as
/// `acting` (`None` for a member token) on the hosted mailspace at `root`.
pub(in crate::mcp) fn pump_control(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_control", "control the pump", acting)?;
    let action = req_str(arguments, "action")?;
    let mode = match action {
        "pause" => PumpMode::Paused,
        "resume" | "start" => PumpMode::Running,
        "stop" => PumpMode::Stopped,
        other => {
            return Err(ToolError::new(format!(
                "action must be pause, resume, stop, or start; got '{other}'"
            )));
        }
    };
    let hosted = authorize("pump_control", root, caller, requested)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    set_project_mode(root, mode).map_err(lib_error)?;
    let home = project_home(root).map_err(lib_error)?;
    let control = inspect(&home).map_err(lib_error)?;
    Ok(json!({
        "action": action,
        "mode": control.mode.to_string(),
        "shutdown_requested": control.shutdown_requested,
        "by": hosted.actor,
    }))
}

/// Run `pump_gate` for `caller` acting as `acting`. `show` reads the gate
/// command; `set` writes it and is rate limited as a write. Both answer
/// `{command}` as it stands afterwards (`null` when unset or blank).
pub(in crate::mcp) fn pump_gate(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_gate", "use the pump gate", acting)?;
    let action = req_str(arguments, "action")?;
    let new_command = match action {
        "show" => None,
        "set" => match arguments.get("command") {
            Some(Value::String(text)) => Some(text.as_str()),
            _ => return Err(ToolError::new("command (a string) is required to set")),
        },
        other => {
            return Err(ToolError::new(format!(
                "action must be show or set; got '{other}'"
            )));
        }
    };
    let hosted = authorize("pump_gate", root, caller, requested)?;
    if let Some(command) = new_command {
        rate_limit(limiter, project_id, &hosted.actor)?;
        gate_config::set_command(root, command).map_err(lib_error)?;
    }
    let command = gate_config::command(root).map_err(lib_error)?;
    Ok(json!({ "command": command }))
}

/// The identity an operator tool acts as: a member token has none and is
/// refused.
fn acting_key<'a>(
    tool: &str,
    purpose: &str,
    acting: Option<&'a str>,
) -> Result<&'a str, ToolError> {
    acting.ok_or_else(|| {
        ToolError::new(format!(
            "{tool} needs the operator's API key; a member token cannot {purpose}"
        ))
    })
}

/// Open the hosted mailspace and require the operator for `tool`.
fn authorize(
    tool: &str,
    root: &Path,
    caller: &IdentityRecord,
    requested: &str,
) -> Result<Hosted, ToolError> {
    let hosted = Hosted::open(root, requested)?;
    require_operator(tool, &hosted, caller)?;
    Ok(hosted)
}

fn require_operator(tool: &str, hosted: &Hosted, caller: &IdentityRecord) -> Result<(), ToolError> {
    let is_host = caller.mailspace_id.as_deref() == Some(hosted.ms.mailspace_id());
    if !is_host {
        return Err(ToolError::new(format!(
            "{tool} needs an API key minted by the host's own member; this key was \
             minted by another member"
        )));
    }
    let kind = hosted.ms.role_view(&hosted.actor).map_err(lib_error)?.kind;
    if !kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("operator"))
    {
        return Err(ToolError::new(format!(
            "{tool} needs a key acting as a role of kind operator; this key acts as '{}'",
            hosted.actor
        )));
    }
    Ok(())
}
