//! `need_show`: the operator's one-need read, built on the generic show.
//!
//! Operator-only with [`super::pump`]'s authorization (the host's own
//! member's API key, acting as a role of kind operator), but a read: it
//! writes nothing and is not rate limited. It resolves the handle with the
//! shared [`super::locate`], refuses anything that is not a need, then
//! answers the generic show body plus the units the backlog graph binds to
//! the need with their node states — the list the CLI's `vivi need show`
//! prints as `units`.
//!
//! [`need_thread`] and [`task_thread`] are the same shape of read for one
//! work item's reply thread: the anchor's state and newest lifecycle event,
//! then a page of the thread's letters oldest-first, newest page by
//! default, paged backwards with a `before` cursor.

use std::path::Path;

use serde_json::{Value, json};

use crate::mailspace::MailspaceThreadMessage;
use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, opt_str, opt_usize, req_str};
use crate::server::safety::RateLimiter;

use super::lib_error;
use super::locate;
use super::pump::{acting_key, authorize};
use super::read;
use super::status_for_role;

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

/// The page bounds the thread tools share: the newest 20 letters by
/// default, at most 100 on one page.
const PAGE_DEFAULT: usize = 20;
const PAGE_MAX: usize = 100;

/// Run `need_thread`: the operator's one need's thread, newest page first.
pub(in crate::server::mcp) fn need_thread(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    work_thread(
        "need_thread",
        "need",
        "read a need's thread",
        root,
        caller,
        acting,
        arguments,
    )
}

/// Run `task_thread`: the operator's one task's thread, newest page first.
pub(in crate::server::mcp) fn task_thread(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    work_thread(
        "task_thread",
        "task",
        "read a task's thread",
        root,
        caller,
        acting,
        arguments,
    )
}

/// The thread read `need_thread` and `task_thread` share: the operator
/// authorization [`need_show`](need_show) uses, the anchor resolved and
/// held to `kind`, then the anchor's complete linked thread — no traversal
/// cap, never re-sorted — paged from its newest letters.
fn work_thread(
    tool: &str,
    kind: &str,
    purpose: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key(tool, purpose, acting)?;
    let handle = req_str(arguments, "handle")?;
    let limit = opt_usize(arguments, "limit", PAGE_DEFAULT, PAGE_MAX)?;
    let before = opt_str(arguments, "before")?;
    let hosted = authorize(tool, root, caller, requested)?;
    let storage = hosted.ms.storage().map_err(lib_error)?;
    let located = locate(&storage, handle)?;
    if located.kind != kind {
        return Err(ToolError::new(format!("not a {kind}: {handle}")));
    }
    let canonical = located.view.handle.clone();
    let messages = hosted
        .ms
        .thread(&canonical, false, usize::MAX, usize::MAX)
        .map_err(lib_error)?;
    let total = messages.len();
    let end = page_end(&messages, before.as_deref())?;
    let start = end.saturating_sub(limit);
    let event = hosted
        .ms
        .item_event_state(&canonical)
        .map_err(lib_error)?
        .map(|state| json!({ "state": state.state, "event": state.event, "heads": state.heads }));
    let mut answer = json!({
        "handle": canonical,
        "state": status_for_role(&located.view.local_role),
        "event": event,
        "messages": messages[start..end].iter().map(read::thread_row).collect::<Vec<Value>>(),
        "total": total,
        "truncated": start > 0,
    });
    if start > 0 {
        answer["next_before"] = json!(messages[start].handle);
    }
    Ok(answer)
}

/// The exclusive end of the page: the whole thread, or the position of the
/// `before` cursor, which must name a message of this very thread.
fn page_end(messages: &[MailspaceThreadMessage], before: Option<&str>) -> Result<usize, ToolError> {
    match before {
        None => Ok(messages.len()),
        Some(cursor) => messages
            .iter()
            .position(|m| m.handle.eq_ignore_ascii_case(cursor) || m.message_id == cursor)
            .ok_or_else(|| {
                ToolError::new(format!(
                    "before: '{cursor}' is not a message of this thread"
                ))
            }),
    }
}
