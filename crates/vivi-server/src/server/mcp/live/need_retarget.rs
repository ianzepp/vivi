//! `need_retarget`: the operator-only tool that moves a need to another
//! repository target after it was sent.
//!
//! The same library call the CLI's `vivi need retarget` makes
//! ([`vivi_core::mailspace::Mailspace::retarget_need`]): the target is
//! validated against the project's `[pump.targets.<id>]` keys and the change
//! is a `retargeted` property letter on the need, so every member that holds
//! the letters reports the same target. Authorization is the pump tools'
//! ([`acting_key`] and [`authorize`]): an API key minted by the host's own
//! member, acting as a role of kind `operator`.

use std::path::Path;

use serde_json::Value;
use vivi_core::mailspace::NeedRetarget;

use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, opt_str, rate_limit, req_str};
use crate::server::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};

/// Run `need_retarget` for `caller` (the key's minting member) acting as
/// `acting` (`None` for a member token, which is refused) on the hosted
/// mailspace at `root`.
pub(in crate::server::mcp) fn need_retarget(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("need_retarget", "retarget a need", acting)?;
    let handle = req_str(arguments, "handle")?;
    let target = req_str(arguments, "target")?;
    let reason = opt_str(arguments, "reason")?;
    let hosted = authorize("need_retarget", root, caller, requested)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    let write: NeedRetarget = hosted
        .ms
        .retarget_need(Some(&hosted.actor), handle, target, reason)
        .map_err(lib_error)?;
    serde_json::to_value(&write).map_err(|error| ToolError::new(error.to_string()))
}
