//! Send/reply composition and recipient parsing for the MCP dispatcher.

use super::*;

pub(super) fn send(
    store: &ProjectStore,
    limiter: &RateLimiter,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &identity.name)?;
    let kind = req_str(arguments, "kind")?;
    if !matches!(kind, "need" | "task" | "want" | "mail" | "chat") {
        return Err(ToolError::new(
            "kind must be need, task, want, chat, or mail",
        ));
    }
    let to = parse_to(arguments)?;
    let subject = req_str(arguments, "subject")?;
    let body = req_str(arguments, "body")?;
    if subject.trim().is_empty() || body.trim().is_empty() {
        return Err(ToolError::new("subject and body must be non-empty"));
    }
    // The sugar arguments build the work letter's front matter, and the
    // dependencies ride in it; the composer gates the body and writes the
    // format header.
    let body = sugared_body(kind, body, arguments)?;
    let composed = compose_send(identity, &to, subject, &body, kind).map_err(ToolError::new)?;
    let outcome = append_composed(store, project_id, identity, composed).map_err(map_project)?;
    Ok(json!({
        "message_id": outcome.message_id,
        "handle": handle_of(&outcome.message_id),
        "sequence": outcome.sequence,
        "kind": kind,
        "duplicate": outcome.duplicate,
    }))
}

pub(super) fn reply(
    store: &ProjectStore,
    limiter: &RateLimiter,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &identity.name)?;
    let handle = req_str(arguments, "handle")?;
    let body = req_str(arguments, "body")?;
    if body.trim().is_empty() {
        return Err(ToolError::new("body must be non-empty"));
    }
    let parent = lookup_one(store, project_id, identity, handle)?;
    let to = parse_to(arguments)?;
    let composed = compose_reply(identity, &parent, &to, body).map_err(ToolError::new)?;
    let outcome = append_composed(store, project_id, identity, composed).map_err(map_project)?;
    Ok(json!({
        "message_id": outcome.message_id,
        "handle": handle_of(&outcome.message_id),
        "in_reply_to": handle,
        "sequence": outcome.sequence,
    }))
}

pub(crate) fn parse_to(arguments: &Value) -> Result<Vec<String>, ToolError> {
    let to = match arguments.get("to") {
        None => Vec::new(),
        Some(Value::String(raw)) => {
            if raw.trim().is_empty() {
                return Err(ToolError::new("to must not be blank"));
            }
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                let value = item
                    .as_str()
                    .ok_or_else(|| ToolError::new("to must be strings"))?;
                if value.trim().is_empty() {
                    return Err(ToolError::new("to entries must not be blank"));
                }
                Ok(value.trim().to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(ToolError::new("to must be a string or a list of strings")),
    };
    Ok(to)
}
