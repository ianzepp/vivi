//! The hosted `send` with `dispatch true`: one authorized shortcut that
//! files the need and hands it to the pump, instead of filing a transport
//! letter.
//!
//! Authorization is the operator tools' ([`acting_key`] and [`authorize`]):
//! an API key minted by the host's own member, acting as a role of kind
//! `operator`. A member token has no acting identity and is refused; no
//! grant level or role name is ever substituted. The send itself is
//! [`write::send`] unchanged — composition, sugar, dependencies, validation,
//! delivery, response fields, and the one write allowance it takes. The
//! dispatch adds exactly one pump record and, when a priority was requested,
//! one priority row at the configured pump home. A failure after the send
//! leaves the filed need in place and names it; nothing is resent, deleted,
//! or rolled back.

use std::path::Path;

use serde_json::{Value, json};

use crate::pump::control::project_home;
use crate::pump::priority;
use crate::server::identities::IdentityRecord;
use crate::server::mcp::send_options::SendOptions;
use crate::server::mcp::tools::ToolError;
use crate::server::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};
use super::write;

/// Run the dispatch form of `send` for `caller` acting as `acting` (`None`
/// for a member token, which is refused) on the hosted mailspace at `root`.
pub(in crate::server::mcp) fn send_dispatch(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    // Revalidated here, so the refusals never depend on the router having
    // parsed the same arguments.
    let options = SendOptions::parse(arguments)?;
    let requested = acting_key("send", "dispatch a send", acting)?;
    let hosted = authorize("send", root, caller, requested)?;
    // A priority resolves the configured home before anything is filed, so a
    // missing [pump] table is an early refusal; a dispatch without one keeps
    // the library defaults and needs no [pump] table.
    let home = match options.priority {
        Some(_) => Some(project_home(root).map_err(lib_error)?),
        None => None,
    };
    let mut out = write::send(&hosted, limiter, project_id, arguments)?;
    let need = out["handle"]
        .as_str()
        .ok_or_else(|| ToolError::new("the send did not return a need handle"))?
        .to_owned();
    let record = hosted.ms.pump_send(None, &need, None).map_err(|error| {
        ToolError::new(format!(
            "need {need} was filed, but its pump record failed: {error}"
        ))
    })?;
    if let (Some(home), Some(level)) = (home, options.priority) {
        priority::set(&home, &record.need, level).map_err(|error| {
            ToolError::new(format!(
                "need {need} was filed with pump record {}, but setting priority \
                 {level} on {} failed: {error}",
                record.handle, record.need
            ))
        })?;
        out["priority"] = json!(level);
    }
    out["pump"] = serde_json::to_value(&record).map_err(lib_error)?;
    Ok(out)
}
