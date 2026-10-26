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

use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, opt_usize, req_str};
use crate::server::safety::RateLimiter;

use super::Hosted;
use super::lib_error;
use super::pump::{acting_key, authorize};

const UNITS_DEFAULT: usize = 20;
const UNITS_MAX: usize = 100;

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
    let located = {
        let storage = hosted.ms.storage().map_err(lib_error)?;
        super::locate(&storage, handle)?
    };
    if located.kind != "need" {
        return Err(ToolError::new(format!(
            "'{handle}' is a {}, not a need",
            located.kind
        )));
    }
    Ok(located.view.handle)
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
