//! Hosted-project writes: the same library calls the CLI makes, as the
//! acting identity.

use std::collections::BTreeMap;

use crate::letter::plain::CloseFront;
use crate::mailspace::{DeliveryResult, SendRequest, folded_marks};
use serde_json::{Value, json};

use crate::server::mcp::tools::{
    ToolError, close_arguments, opt_str, parse_to, rate_limit, req_str, sugared_body,
};
use crate::server::safety::RateLimiter;

use super::read::{memo_items, string_list};
use super::{Hosted, is_work, lib_error, locate};

/// A delivery as the tool reports it: the handle of the item as the
/// recipient holds it (the sender's own copy when nothing was delivered
/// locally), and every copy.
fn delivery_json(h: &Hosted, result: &DeliveryResult) -> Result<Value, ToolError> {
    let handle = result
        .delivered
        .first()
        .map_or(result.sent.as_str(), |d| d.handle.as_str());
    let storage = h.ms.storage().map_err(lib_error)?;
    let message_id = storage.resolve_message_token(handle).map_err(lib_error)?;
    Ok(json!({
        "handle": handle,
        "message_id": message_id,
        "sent": result.sent,
        "delivered": result
            .delivered
            .iter()
            .map(|d| json!({ "identity": d.identity, "handle": d.handle }))
            .collect::<Vec<_>>(),
        "remote": result.remote,
        "from": h.actor,
    }))
}

pub(super) fn send(
    h: &Hosted,
    limiter: &RateLimiter,
    project_id: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &h.actor)?;
    let kind = req_str(arguments, "kind")?;
    let role = match kind {
        "need" => "needs",
        "task" => "tasks",
        "want" => "wants",
        "mail" => "inbox",
        "chat" => {
            return Err(ToolError::new(
                "chat is not a kind of a hosted project; send mail instead",
            ));
        }
        _ => return Err(ToolError::new("kind must be need, task, want, or mail")),
    };
    let to = parse_to(arguments)?;
    let subject = req_str(arguments, "subject")?;
    let body = req_str(arguments, "body")?;
    // The sugar arguments build the work letter's front matter, as the
    // CLI's flags do; vivi's send gate re-checks the built letter, so its
    // refusal text reaches the caller unchanged.
    let body = sugared_body(kind, body, arguments)?;
    let result =
        h.ms.send(SendRequest {
            from: h.actor.clone(),
            to,
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_owned(),
            body,
            role: role.into(),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .map_err(lib_error)?;
    let mut out = delivery_json(h, &result)?;
    out["kind"] = json!(kind);
    Ok(out)
}

pub(super) fn reply(
    h: &Hosted,
    limiter: &RateLimiter,
    project_id: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &h.actor)?;
    let handle = req_str(arguments, "handle")?;
    let body = req_str(arguments, "body")?;
    let result =
        h.ms.reply(
            handle,
            &h.actor,
            Vec::new(),
            Vec::new(),
            None,
            body.to_owned(),
        )
        .map_err(lib_error)?;
    let mut out = delivery_json(h, &result)?;
    out["in_reply_to"] = json!(handle);
    Ok(out)
}

/// `done` moves the item to the done folder; `reopen` moves it back to the
/// folder of its kind, as `need|task done|reopen` do.
pub(super) fn settle(
    h: &Hosted,
    limiter: &RateLimiter,
    project_id: &str,
    arguments: &Value,
    done: bool,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &h.actor)?;
    let handle = req_str(arguments, "handle")?;
    let note = opt_str(arguments, "note")?;
    let kind = {
        let storage = h.ms.storage().map_err(lib_error)?;
        locate(&storage, handle)?.kind
    };
    if !is_work(&kind) {
        return Err(ToolError::new(format!(
            "{handle} is not a need, task, or want"
        )));
    }
    if !done && kind == "want" {
        // The CLI has `want done` and `drop`, and no reopen.
        return Err(ToolError::new(format!(
            "{handle} is a want; a want has no reopen (send a need that serves it (`serves`) \
             instead)"
        )));
    }
    let (role, verb) = if done {
        ("done", "done")
    } else {
        (open_role(&kind), "reopen")
    };
    // Close metadata rides the event letter's front matter, as the CLI's
    // --verdict/--repo/--tip/--branch/--commit flags do; a reopen carries
    // none (the facts belong to its done event).
    let close = close_arguments(arguments)?;
    if !done && close.is_some() {
        return Err(ToolError::new(
            "reopen takes no close metadata: the facts belong to the done event",
        ));
    }
    let metadata = close.map(close_metadata);
    let outcome =
        h.ms.move_item_outcome(
            &h.actor,
            handle,
            role,
            note,
            &format!("{kind} {verb}"),
            metadata.as_ref(),
        )
        .map_err(lib_error)?;
    let mode = match (&outcome.event, outcome.reported, done) {
        (Some(_), _, true) => "closed",
        (Some(_), _, false) => "reopened",
        (None, true, _) => "reported",
        (None, false, _) => "moved",
    };
    Ok(json!({
        "handle": outcome.handle,
        "mode": mode,
        "event": outcome.event,
        "reported": outcome.reported,
        "joined_needs": outcome.joined_needs,
    }))
}

fn open_role(kind: &str) -> &'static str {
    match kind {
        "need" => "needs",
        "task" => "tasks",
        _ => "wants",
    }
}

/// Close metadata in the library's `move_item_outcome` shape: `verdict`,
/// `branch`, `commit`, and one `repo:<name>` key per repo tip.
fn close_metadata(close: CloseFront) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    if let Some(verdict) = close.verdict {
        metadata.insert("verdict".to_owned(), verdict);
    }
    if let Some(branch) = close.branch {
        metadata.insert("branch".to_owned(), branch);
    }
    if let Some(commit) = close.commit {
        metadata.insert("commit".to_owned(), commit);
    }
    for (repo, tip) in close.repos {
        metadata.insert(format!("repo:{repo}"), tip);
    }
    metadata
}

pub(super) fn memo(
    h: &Hosted,
    limiter: &RateLimiter,
    project_id: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    match opt_str(arguments, "action")?.unwrap_or("list") {
        "list" => Ok(json!({ "items": memo_items(h, Some(&h.actor))? })),
        "show" => {
            let handle = req_str(arguments, "handle")?;
            memo_items(h, Some(&h.actor))?
                .into_iter()
                .find(|item| {
                    item["handle"].as_str() == Some(handle)
                        || item["message_id"].as_str() == Some(handle)
                })
                .ok_or_else(|| ToolError::new(format!("memo not found: {handle}")))
        }
        "save" => {
            rate_limit(limiter, project_id, &h.actor)?;
            let subject = req_str(arguments, "subject")?;
            let body = req_str(arguments, "body")?;
            let tags = string_list(arguments, "tags")?;
            let handle =
                h.ms.save_memo(&h.actor, subject, body, &tags)
                    .map_err(lib_error)?;
            let message_id =
                h.ms.storage()
                    .map_err(lib_error)?
                    .resolve_message_token(&handle)
                    .map_err(lib_error)?;
            Ok(json!({ "handle": handle, "message_id": message_id, "subject": subject }))
        }
        other => Err(ToolError::new(format!(
            "memo action must be list, show, or save; got '{other}'"
        ))),
    }
}

/// `need|task|want depend`: add and remove prerequisites of an item the
/// acting identity may change, as the CLI does. The library refuses a role
/// that may not close the item, an unknown handle, a self edge or a cycle,
/// and writes nothing then.
pub(super) fn depend(
    h: &Hosted,
    limiter: &RateLimiter,
    project_id: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &h.actor)?;
    let handle = req_str(arguments, "handle")?;
    let add = string_list(arguments, "add")?;
    let remove = string_list(arguments, "remove")?;
    let kind = {
        let storage = h.ms.storage().map_err(lib_error)?;
        locate(&storage, handle)?.kind
    };
    if !is_work(&kind) {
        return Err(ToolError::new(format!(
            "{handle} is not a need, a task, or a want"
        )));
    }
    let writes =
        h.ms.set_dependencies(&h.actor, handle, &kind, &add, &remove)
            .map_err(lib_error)?;
    let dependencies = h.ms.backlog_dependencies(handle).map_err(lib_error)?;
    Ok(json!({
        "handle": handle,
        "changes": writes
            .iter()
            .map(|w| json!({
                "dependency": w.handle,
                "action": if w.added { "added" } else { "removed" },
                "changed": w.changed,
            }))
            .collect::<Vec<_>>(),
        "dependencies": dependencies
            .iter()
            .map(|d| json!({ "handle": d.handle, "state": d.state }))
            .collect::<Vec<_>>(),
    }))
}

/// `need|want mark|unmark`: append mark events on an item the acting
/// identity holds.
pub(super) fn mark(
    h: &Hosted,
    limiter: &RateLimiter,
    project_id: &str,
    arguments: &Value,
    add: bool,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &h.actor)?;
    let handle = req_str(arguments, "handle")?;
    let slugs = string_list(arguments, "marks")?;
    let kind = {
        let storage = h.ms.storage().map_err(lib_error)?;
        locate(&storage, handle)?.kind
    };
    if !matches!(kind.as_str(), "need" | "want") {
        return Err(ToolError::new(format!("{handle} is not a need or a want")));
    }
    let writes =
        h.ms.set_marks(&h.actor, handle, &kind, add, &slugs)
            .map_err(lib_error)?;
    let storage = h.ms.storage().map_err(lib_error)?;
    let loc = locate(&storage, handle)?;
    Ok(json!({
        "handle": writes.first().map_or(handle, |w| w.handle.as_str()),
        "action": if add { "marked" } else { "unmarked" },
        "changes": writes
            .iter()
            .map(|w| json!({ "slug": w.slug, "changed": w.changed }))
            .collect::<Vec<_>>(),
        "marks": folded_marks(&loc.events),
    }))
}
