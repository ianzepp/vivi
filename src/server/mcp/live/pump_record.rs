//! `pump_send`, `pump_list` and `pump_show`: operator-only pump records on a
//! hosted project.
//!
//! The same library calls the CLI's `vivi pump send|list|show` make
//! ([`Mailspace::pump_send`], [`Mailspace::pump_list`],
//! [`Mailspace::pump_show`]). A record is how a need is handed to the pump:
//! `pump_send` files one open record, which the pump reads on its next tick.
//!
//! Authorization is `pump_control`'s (one shared check in `pump.rs`): an API
//! key minted by the host's own member, acting as a role of kind `operator`.
//! Unlike the CLI, the body is literal text only: the CLI's `-` (stdin) and
//! `@path` forms would read the server's own files.

use std::path::Path;

use crate::mailspace::PumpState;
use serde_json::{Value, json};

use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, opt_str, opt_usize, rate_limit, req_str};
use crate::server::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};

const LIST_DEFAULT: usize = 50;
const LIST_MAX: usize = 100;

/// File one open pump record for a need. Rate limited as a write.
pub(in crate::server::mcp) fn pump_send(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_send", "send pump records", acting)?;
    let need = req_str(arguments, "need")?;
    let from = opt_str(arguments, "from")?;
    let body = opt_str(arguments, "body")?;
    let hosted = authorize("pump_send", root, caller, requested)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    let record = hosted.ms.pump_send(from, need, body).map_err(lib_error)?;
    serde_json::to_value(&record).map_err(lib_error)
}

/// List pump records, oldest first. At most `limit` are returned (the newest
/// ones when more match); `total` counts every match.
pub(in crate::server::mcp) fn pump_list(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_list", "list pump records", acting)?;
    let need = opt_str(arguments, "need")?;
    let state = match opt_str(arguments, "status")?.unwrap_or("open") {
        "open" => Some(PumpState::Open),
        "done" => Some(PumpState::Done),
        "all" => None,
        other => {
            return Err(ToolError::new(format!(
                "status must be open, done, or all; got '{other}'"
            )));
        }
    };
    let limit = opt_usize(arguments, "limit", LIST_DEFAULT, LIST_MAX)?;
    let hosted = authorize("pump_list", root, caller, requested)?;
    let mut records = hosted.ms.pump_list(need, state).map_err(lib_error)?;
    let total = records.len();
    records.drain(..total.saturating_sub(limit));
    Ok(json!({
        "records": records,
        "total": total,
        "truncated": total > records.len(),
    }))
}

/// One pump record and its body.
pub(in crate::server::mcp) fn pump_show(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pump_show", "read pump records", acting)?;
    let handle = req_str(arguments, "handle")?;
    let hosted = authorize("pump_show", root, caller, requested)?;
    let (record, body) = hosted.ms.pump_show(handle).map_err(lib_error)?;
    let mut value = serde_json::to_value(&record).map_err(lib_error)?;
    value["body"] = Value::String(body);
    Ok(value)
}
