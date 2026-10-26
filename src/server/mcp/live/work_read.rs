//! `need_show`: the operator's one-need read, built on the generic show.
//!
//! Operator-only with [`super::pump`]'s authorization (the host's own
//! member's API key, acting as a role of kind operator), but a read: it
//! writes nothing and is not rate limited. It resolves the handle with the
//! shared [`super::locate`], refuses anything that is not a need, then
//! answers the generic show body plus the units the backlog graph binds to
//! the need with their node states — the list the CLI's `vivi need show`
//! prints as `units`.

use std::path::Path;

use serde_json::{Value, json};

use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, req_str};
use crate::server::safety::RateLimiter;

use super::lib_error;
use super::locate;
use super::pump::{acting_key, authorize};
use super::read;

/// Run `need_show` for `caller` (the key's minting member) acting as
/// `acting` (`None` for a member token) on the hosted mailspace at `root`.
pub(in crate::server::mcp) fn need_show(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("need_show", "read a need", acting)?;
    let handle = req_str(arguments, "handle")?;
    let hosted = authorize("need_show", root, caller, requested)?;
    let storage = hosted.ms.storage().map_err(lib_error)?;
    let located = locate(&storage, handle)?;
    if located.kind != "need" {
        return Err(ToolError::new(format!("not a need: {handle}")));
    }
    let canonical = located.view.handle.clone();
    let units = hosted
        .ms
        .backlog_bound_units(&canonical)
        .map_err(lib_error)?;
    let mut answer = read::show_located(&hosted, located)?;
    answer["units"] = json!(
        units
            .iter()
            .map(|unit| json!({ "handle": unit.handle, "state": unit.state }))
            .collect::<Vec<Value>>()
    );
    Ok(answer)
}
