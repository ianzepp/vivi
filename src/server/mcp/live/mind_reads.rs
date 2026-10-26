//! Operator-only reads of the hosted backlog's structure: `need_units`, the
//! units bound to a need, as `vivi need show` reads them.
//!
//! Authorization is `pump_control`'s (one shared check in `pump.rs`): an API
//! key minted by the host's own member, acting as a role of kind `operator`.
//! Unlike the pump writes these tools only read: no audit line and no write
//! rate limit — the server's write classification already leaves them out,
//! and they never touch the limiter themselves.

use std::path::Path;

use serde_json::{Value, json};

use crate::mailspace::MailspaceThreadMessage;
use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, opt_usize, req_str};
use crate::server::safety::RateLimiter;

use super::Hosted;
use super::lib_error;
use super::pump::{acting_key, authorize};
use super::task_evidence::{self, AttemptRow, MessageRow, TaskEvidence, Window};

const UNITS_DEFAULT: usize = 20;
const UNITS_MAX: usize = 100;
const THREAD_DEFAULT: usize = 20;
const THREAD_MAX: usize = 50;

/// The units bound to a need, paged. Read-only: neither audited nor
/// write-rate-limited.
pub(in crate::server::mcp) fn need_units(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("need_units", "read bound need units", acting)?;
    let need = req_str(arguments, "need")?;
    let hosted = authorize("need_units", root, caller, requested)?;
    let canonical = canonical_need(&hosted, need)?;
    let units = hosted
        .ms
        .backlog_bound_units(&canonical)
        .map_err(lib_error)?;
    let total = units.len();
    let page = page(arguments, total, UNITS_DEFAULT, UNITS_MAX)?;
    let rows = units[page.start..page.end]
        .iter()
        .map(|unit| json!({"handle": unit.handle, "state": unit.state}))
        .collect::<Vec<_>>();
    Ok(json!({
        "need": canonical,
        "units": rows,
        "total": total,
        "offset": page.offset,
        "limit": page.limit,
        "next_offset": page.next_offset,
        "truncated": page.truncated,
    }))
}

/// The canonical handle of the need `handle` names. An unknown handle, and a
/// handle for any other kind, is refused here — so an empty `units` answer
/// below always means a real need with no bound units, and a priority set
/// always lands on a need, never on another kind.
pub(super) fn canonical_need(hosted: &Hosted, handle: &str) -> Result<String, ToolError> {
    canonical_kind(hosted, handle, "need")
}

/// The canonical handle of the item `handle` names when it is a `kind`. An
/// unknown handle, and a handle of any other kind, is refused — so a read
/// of a real item always answers with that item, never with another kind.
fn canonical_kind(hosted: &Hosted, handle: &str, kind: &str) -> Result<String, ToolError> {
    let located = {
        let storage = hosted.ms.storage().map_err(lib_error)?;
        super::locate(&storage, handle)?
    };
    if located.kind != kind {
        return Err(ToolError::new(format!(
            "'{handle}' is a {}, not a {kind}",
            located.kind
        )));
    }
    Ok(located.view.handle)
}

/// One task's diagnostic thread: its complete connected thread cut by the
/// evidence helpers into the paged messages and the separately paged
/// newest-first attempts. Read-only: neither audited nor write-rate-limited.
pub(in crate::server::mcp) fn task_thread(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("task_thread", "read a task's diagnostic thread", acting)?;
    let step = req_str(arguments, "step")?;
    let hosted = authorize("task_thread", root, caller, requested)?;
    let task = canonical_kind(&hosted, &step, "task")?;
    let window = evidence_window(arguments)?;
    let thread = complete_thread(&hosted, &task)?;
    let evidence = task_evidence::task_evidence(&thread, window, window);
    Ok(task_thread_json(&task, &evidence))
}

/// The task's complete connected thread, uncapped: selecting and paging is
/// the evidence helpers' job, not the generic thread read's.
fn complete_thread(hosted: &Hosted, task: &str) -> Result<Vec<MailspaceThreadMessage>, ToolError> {
    hosted
        .ms
        .thread(task, false, usize::MAX, usize::MAX)
        .map_err(lib_error)
}

/// The one window both pages share: `limit` (default 20, at most 50) and
/// `offset` (default 0), as [`need_units`] names them.
fn evidence_window(arguments: &Value) -> Result<Window, ToolError> {
    Ok(Window {
        offset: opt_offset(arguments)?,
        limit: opt_usize(arguments, "limit", THREAD_DEFAULT, THREAD_MAX)?,
    })
}

/// The answer: the canonical task, the diagnostic attempt evidence with its
/// receipt first, the verbose paged messages last — so a downstream cap on
/// the serialized answer cannot conceal the evidence.
fn task_thread_json(task: &str, evidence: &TaskEvidence) -> Value {
    json!({
        "task": task,
        "attempts": evidence.attempts.iter().map(attempt_json).collect::<Vec<_>>(),
        "attempts_total": evidence.attempts_total,
        "attempts_offset": evidence.attempts_offset,
        "attempts_limit": evidence.attempts_limit,
        "attempts_next_offset": evidence.attempts_next_offset,
        "attempts_truncated": evidence.attempts_truncated,
        "messages": evidence.messages.iter().map(message_json).collect::<Vec<_>>(),
        "total": evidence.total,
        "offset": evidence.offset,
        "limit": evidence.limit,
        "next_offset": evidence.next_offset,
        "truncated": evidence.truncated,
    })
}

/// One attempt row: its source handles, its parsed record metadata, and the
/// bounded body that leads with the failure evidence, with what was cut.
fn attempt_json(row: &AttemptRow) -> Value {
    json!({
        "handle": row.handle,
        "message_id": row.message_id,
        "date": row.date,
        "metadata": row.fields,
        "body": row.body,
        "body_truncated": row.body_truncated,
    })
}

/// One thread message row in thread order: its source handles, the lifecycle
/// event when it is one, its capped body, and whether it is an attempt note.
fn message_json(row: &MessageRow) -> Value {
    json!({
        "handle": row.handle,
        "message_id": row.message_id,
        "date": row.date,
        "from": row.from,
        "subject": row.subject,
        "event": row.event,
        "body": row.body,
        "body_truncated": row.body_truncated,
        "is_attempt": row.is_attempt,
    })
}

/// One page over a full result list: the slice bounds plus the paging
/// receipt the tool answers with. `next_offset` is set only when the page
/// left rows out; an offset at or past the end is the empty terminal page.
pub(super) struct Page {
    /// First row of the page in the full list.
    pub(super) start: usize,
    /// One past the last row of the page.
    pub(super) end: usize,
    /// The offset the call named (0 when absent).
    pub(super) offset: usize,
    /// The effective limit: the asked-for one capped at the maximum.
    pub(super) limit: usize,
    /// The next page's offset, when more rows follow.
    pub(super) next_offset: Option<usize>,
    /// Whether any row was left out.
    pub(super) truncated: bool,
}

/// Cut `total` rows into the page `arguments` names. `limit` defaults to
/// `default_limit` and caps at `max_limit` ([`opt_usize`]); `offset` is a
/// plain non-negative count, 0 allowed — which `opt_usize` refuses.
pub(super) fn page(
    arguments: &Value,
    total: usize,
    default_limit: usize,
    max_limit: usize,
) -> Result<Page, ToolError> {
    let limit = opt_usize(arguments, "limit", default_limit, max_limit)?;
    let offset = opt_offset(arguments)?;
    let (start, end) = if offset >= total {
        (total, total)
    } else {
        (
            offset,
            offset.checked_add(limit).unwrap_or(total).min(total),
        )
    };
    let truncated = end < total;
    Ok(Page {
        start,
        end,
        offset,
        limit,
        next_offset: truncated.then_some(end),
        truncated,
    })
}

/// A paging offset: absent is 0, and 0 is a valid start.
fn opt_offset(arguments: &Value) -> Result<usize, ToolError> {
    let bad = || ToolError::new("offset must be a non-negative integer");
    match arguments.get("offset") {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => {
            let parsed = n.as_u64().ok_or_else(bad)?;
            usize::try_from(parsed).map_err(|_| bad())
        }
        _ => Err(bad()),
    }
}
