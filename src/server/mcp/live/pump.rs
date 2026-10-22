//! `pump_control`: operator-only pump control for a hosted project.
//!
//! The same library call the CLI's `vivi pump pause|resume|stop|start`
//! makes ([`vivi::pump::control::set_project_mode`]); the control state
//! lives in the host's `pump.db`, not in the server. In the CLI `start` is
//! `resume`: both set the mode to running.
//!
//! The caller must be the operator:
//!
//! 1. an API key (a member token never reaches a hosted mailspace),
//! 2. minted by the host's own member (the creator registered for the
//!    mailspace's own sync key, as `vivi sync api-key mint` does), so a key
//!    another member minted for itself cannot steer the pump, and
//! 3. acting as a role of kind `operator`.

use std::path::Path;

use serde_json::{Value, json};
use vivi::pump::control::{PumpMode, inspect, project_home, set_project_mode};

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
    let Some(requested) = acting else {
        return Err(ToolError::new(
            "pump_control needs the operator's API key; a member token cannot control the pump",
        ));
    };
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
    let hosted = Hosted::open(root, requested)?;
    require_operator(&hosted, caller)?;
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

fn require_operator(hosted: &Hosted, caller: &IdentityRecord) -> Result<(), ToolError> {
    let is_host = caller.mailspace_id.as_deref() == Some(hosted.ms.mailspace_id());
    if !is_host {
        return Err(ToolError::new(
            "pump_control needs an API key minted by the host's own member; this key was \
             minted by another member",
        ));
    }
    let kind = hosted.ms.role_view(&hosted.actor).map_err(lib_error)?.kind;
    if !kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("operator"))
    {
        return Err(ToolError::new(format!(
            "pump_control needs a key acting as a role of kind operator; this key acts as '{}'",
            hosted.actor
        )));
    }
    Ok(())
}
