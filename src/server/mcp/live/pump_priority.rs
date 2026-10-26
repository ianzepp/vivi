//! `pump_priority`: the operator-only per-need dispatch priority tool for a
//! hosted project.
//!
//! The same library calls the CLI's `vivi pump priority set|clear|list` makes
//! ([`crate::pump::priority::set`], [`crate::pump::priority::clear`],
//! [`crate::pump::priority::list`]): rows in the configured pump home's
//! `pump.db` (`need_priority`), which the running pump reads each tick.
//! Priority is live operator state, not a guarded config edit, so the pump
//! may be running and no config write guard applies; `set` and `clear` are
//! rate limited as writes (the server route's accounting has already
//! classified them), and `list` is a read.
//!
//! Keys stay literal: the need is never looked up, canonicalized, or
//! required to exist, so an operator can pre-seed a priority for a need that
//! is only sent later. Level 0 removes the row; a clear of a key without one
//! still succeeds.
//!
//! Authorization is `pump_control`'s (one shared check in `pump.rs`): an API
//! key minted by the host's own member, acting as a role of kind `operator`.

use std::path::Path;

use serde_json::{Value, json};

use crate::pump::control::project_home;
use crate::pump::priority;
use crate::server::identities::IdentityRecord;
use crate::server::mcp::send_options::non_negative_integer;
use crate::server::mcp::tools::{ToolError, rate_limit, req_str};
use crate::server::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};

/// Run `pump_priority` for `caller` (the key's minting member) acting as
/// `acting` (`None` for a member token) on the hosted mailspace at `root`.
pub(in crate::server::mcp) fn pump_priority(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_priority", "set pump priorities", acting)?;
    let action = req_str(arguments, "action")?;
    match action {
        "set" => set(limiter, project_id, root, caller, requested, arguments),
        "clear" => clear(limiter, project_id, root, caller, requested, arguments),
        "list" => list(root, caller, requested, arguments),
        other => Err(ToolError::new(format!(
            "action must be set, clear, or list; got '{other}'"
        ))),
    }
}

/// Store `need` at `level`; level 0 removes the row.
fn set(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    requested: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let need = req_str(arguments, "need")?;
    let level = match arguments.get("level") {
        Some(value) => non_negative_integer(value, "level")?,
        None => return Err(ToolError::new("level is required to set")),
    };
    let hosted = authorize("pump_priority", root, caller, requested)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    let home = project_home(root).map_err(lib_error)?;
    priority::set(&home, need, level).map_err(lib_error)?;
    Ok(json!({ "action": "set", "need": need, "level": level }))
}

/// Remove `need`'s priority row; an absent row still succeeds.
fn clear(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    requested: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let need = req_str(arguments, "need")?;
    if arguments.get("level").is_some() {
        return Err(ToolError::new("clear takes no level"));
    }
    let hosted = authorize("pump_priority", root, caller, requested)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    let home = project_home(root).map_err(lib_error)?;
    priority::clear(&home, need).map_err(lib_error)?;
    Ok(json!({ "action": "clear", "need": need, "level": 0 }))
}

/// Every prioritized need, highest level first, then by need handle.
fn list(
    root: &Path,
    caller: &IdentityRecord,
    requested: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    for name in ["need", "level"] {
        if arguments.get(name).is_some() {
            return Err(ToolError::new(format!("list takes no {name}")));
        }
    }
    let hosted = authorize("pump_priority", root, caller, requested)?;
    let home = project_home(root).map_err(lib_error)?;
    let priorities: Vec<Value> = priority::list(&home)
        .map_err(lib_error)?
        .into_iter()
        .map(|(need, level)| json!({ "need": need, "level": level }))
        .collect();
    Ok(json!({ "priorities": priorities }))
}
