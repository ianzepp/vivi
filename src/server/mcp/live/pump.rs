//! `pump_control` and `pump_rungs` (and the authorization the pump record
//! tools in `pump_record.rs` share): operator-only pump tools for a hosted
//! project.
//!
//! The same library call the CLI's `vivi pump pause|resume|stop|start`
//! makes ([`vivi::pump::control::set_project_mode`]); the control state
//! lives in the host's `pump.db`, not in the server. In the CLI `start` is
//! `resume`: both set the mode to running.
//!
//! `pump_rungs` shows or sets the rung commands (`[pump.rungs]`, or
//! `[pump.targets.<id>.rungs]` for one target) through
//! [`vivi::pump::config::RungsConfig`]. `RungsConfig::set` is a guarded write.
//! This tool edits rung commands only; config, pool allow lists and targets
//! are the config write tools.
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
use vivi::mailspace::Mailspace;
use vivi::pump::config::RungsConfig;
use vivi::pump::control::{PumpMode, inspect, project_home, set_project_mode};

use crate::identities::IdentityRecord;
use crate::mcp::tools::{ToolError, opt_str, rate_limit, req_str};
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

/// Run `pump_rungs` for `caller` acting as `acting`. `show` reads the rung
/// table; `set` writes one rung command (workspace level, or the named
/// target's) and is rate limited as a write. Both answer the effective table
/// as it stands afterwards: `{target, task, need, main, release, timeout,
/// pools}`, with `null` for an unset command.
pub(in crate::mcp) fn pump_rungs(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_rungs", "use the pump rungs", acting)?;
    let action = req_str(arguments, "action")?;
    let target = opt_str(arguments, "target")?;
    let change = match action {
        "show" => None,
        "set" => {
            let rung = match arguments.get("rung") {
                Some(Value::String(text)) => text.as_str(),
                _ => return Err(ToolError::new("rung (a string) is required to set")),
            };
            let command = match arguments.get("command") {
                Some(Value::String(text)) => text.as_str(),
                _ => return Err(ToolError::new("command (a string) is required to set")),
            };
            Some((rung, command))
        }
        other => {
            return Err(ToolError::new(format!(
                "action must be show or set; got '{other}'"
            )));
        }
    };
    let hosted = authorize("pump_rungs", root, caller, requested)?;
    if let Some((rung, command)) = change {
        rate_limit(limiter, project_id, &hosted.actor)?;
        RungsConfig::set(root, rung, command, target).map_err(lib_error)?;
    }
    let ms = Mailspace::open_root(root.to_path_buf()).map_err(lib_error)?;
    let cfg = ms.config.pump.unwrap_or_default();
    if let Some(id) = target
        && !cfg.targets.contains_key(id)
    {
        return Err(ToolError::new(format!("unknown pump target `{id}`")));
    }
    let mut answer = serde_json::to_value(cfg.effective_rungs(target))
        .map_err(|error| ToolError::new(error.to_string()))?;
    answer["target"] = json!(target);
    Ok(answer)
}

/// The identity an operator tool acts as: a member token has none and is
/// refused.
pub(super) fn acting_key<'a>(
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
pub(super) fn authorize(
    tool: &str,
    root: &Path,
    caller: &IdentityRecord,
    requested: &str,
) -> Result<Hosted, ToolError> {
    let hosted = Hosted::open(root, requested)?;
    require_operator(tool, &hosted, caller)?;
    Ok(hosted)
}

pub(super) fn require_operator(
    tool: &str,
    hosted: &Hosted,
    caller: &IdentityRecord,
) -> Result<(), ToolError> {
    if !super::is_own_member(&hosted.ms, caller) {
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
