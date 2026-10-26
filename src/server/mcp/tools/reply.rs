//! The reply tool on a server-owned project: it composes through
//! `compose_reply` and appends as the authenticated caller, addressed to
//! the parent letter's sender. The hosted-only explicit `to` is refused
//! here, so a caller is never silently ignored about where a letter goes.

use serde_json::{Value, json};

use crate::server::identities::IdentityRecord;
use crate::server::projects::{ProjectStore, Viewer};
use crate::server::safety::RateLimiter;

use super::{ToolError, lookup_one, map_project, rate_limit, req_str};
use crate::server::mcp::letters::{append_composed, compose_reply, handle_of};

pub(super) fn reply(
    store: &ProjectStore,
    limiter: &RateLimiter,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &identity.name)?;
    // `to` is a hosted-project option; a server-owned reply always goes to
    // the letter's sender, so an explicit value is refused rather than
    // silently dropped.
    if arguments.get("to").is_some_and(|value| !value.is_null()) {
        return Err(ToolError::new(
            "to is available on a hosted project only; a server-owned reply goes to the letter's sender",
        ));
    }
    let handle = req_str(arguments, "handle")?;
    let body = req_str(arguments, "body")?;
    if body.trim().is_empty() {
        return Err(ToolError::new("body must be non-empty"));
    }
    let parent = lookup_one(store, project_id, identity, handle)?;
    let composed = compose_reply(identity, &parent, body).map_err(ToolError::new)?;
    let outcome = append_composed(store, project_id, identity, composed).map_err(map_project)?;
    Ok(json!({
        "message_id": outcome.message_id,
        "handle": handle_of(&outcome.message_id),
        "in_reply_to": handle,
        "sequence": outcome.sequence,
    }))
}
