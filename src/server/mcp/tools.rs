//! Curated MCP tools. Reads use the transport index (visibility) plus
//! vivi's mailspace store for memos. Writes compose through
//! `crate::message::build_compose_draft` and append as the authenticated
//! caller — the server mints the traveling id (decided item 15).

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::letter::plain::{CloseFront, MemoFront, render as render_front};
use crate::letter::sugar::opens_with_fence;
use crate::letter::{Front, of_message};
use crate::message::{render_message, to_json_message};
use crate::storage::{MessageIngestRequest, Storage};
use serde_json::{Value, json};

use crate::server::identities::IdentityRecord;
use crate::server::projects::{ProjectError, ProjectRecord, ProjectStore, Scan, Viewer};
use crate::server::safety::RateLimiter;

use super::letters::{
    EventFold, ItemState, append_composed, caller_address, compose_event, compose_reply,
    compose_send, handle_of, item_status, parse_headers, parse_letter, state_of, valid_handle,
};
use super::live::string_list;

const LIST_DEFAULT: usize = 50;
const LIST_MAX: usize = 100;
const THREAD_DEFAULT: usize = 20;
const THREAD_MAX: usize = 50;

pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub fn dispatch(
    store: &ProjectStore,
    limiter: &RateLimiter,
    project_id: &str,
    identity: &IdentityRecord,
    // The vivi identity the caller's API key was minted to act as. The
    // hosted-project tools (`live`) read it before this dispatcher is
    // reached; a server-owned project acts as `identity` and ignores it.
    _acting_as: Option<&str>,
    name: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    match name {
        "overview" => overview(store, project_id, identity),
        "list" => list_items(store, project_id, identity, arguments),
        "show" => show(store, project_id, identity, arguments),
        "thread" => thread(store, project_id, identity, arguments),
        "send" => send(store, limiter, project_id, identity, arguments),
        "reply" => reply(store, limiter, project_id, identity, arguments),
        "done" => settle(store, limiter, project_id, identity, arguments, "done"),
        "reopen" => settle(store, limiter, project_id, identity, arguments, "reopened"),
        "memo" => memo(store, limiter, project_id, identity, arguments),
        "graph" => graph(store, project_id, identity, arguments),
        "mark" | "unmark" | "depend" | "board" => Err(ToolError::new(format!(
            "{name} is available on a hosted project to an API key only"
        ))),
        other => Err(ToolError::new(format!("unknown tool '{other}'"))),
    }
}

/// Ready handles an overview lists; `ready_total` carries the full count.
pub(super) const OVERVIEW_READY_CAP: usize = 50;

fn overview(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
) -> Result<Value, ToolError> {
    // Event letters are not work and not mail: they only move item state, so
    // the view holds none of them.
    let View { records, states } = read_view(store, project_id, identity, Scan::All, false)?;
    let mut needs_open = 0;
    let mut tasks_open = 0;
    let mut wants_open = 0;
    let mut mail = 0;
    let mut recent = Vec::new();
    for record in records.iter().rev().take(8) {
        recent.push(summary(record, &states));
    }
    recent.reverse();
    for record in &records {
        match record.kind.as_deref() {
            Some("need") if item_is_open(record, &states) => needs_open += 1,
            Some("task") if item_is_open(record, &states) => tasks_open += 1,
            Some("want") if item_is_open(record, &states) => wants_open += 1,
            Some("chat") => mail += 1,
            None => mail += 1,
            _ => {}
        }
    }
    let mut ready = ready_handles(&records, &states);
    let ready_total = ready.len();
    ready.truncate(OVERVIEW_READY_CAP);
    let memos = list_memos(store, project_id, identity)?.len();
    Ok(json!({
        "you": {
            "name": identity.name,
            "mailspace_id": identity.mailspace_id,
            "prefix": identity.mailspace_prefix(),
            "address": caller_address(identity).ok(),
        },
        "project_id": project_id,
        "totals": {
            "needs_open": needs_open,
            "tasks_open": tasks_open,
            "wants_open": wants_open,
            "mail_visible": mail,
            "memos": memos,
            "ready": ready_total,
        },
        "ready": ready,
        "ready_total": ready_total,
        "recent": recent,
    }))
}

fn list_items(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let kind = opt_str(arguments, "kind")?;
    let status = opt_str(arguments, "status")?.unwrap_or("open");
    if !matches!(status, "open" | "done" | "all") {
        return Err(ToolError::new("status must be open, done, or all"));
    }
    let limit = opt_usize(arguments, "limit", LIST_DEFAULT, LIST_MAX)?;
    if kind.as_deref() == Some("memo") {
        let mut memos = list_memos(store, project_id, identity)?;
        memos.truncate(limit);
        return Ok(json!({ "kind": "memo", "items": memos }));
    }
    let wanted_kind = match kind.as_deref() {
        None => None,
        Some("need" | "task" | "want" | "mail" | "chat") => kind.as_deref(),
        Some(other) => {
            return Err(ToolError::new(format!(
                "kind must be need, task, want, chat, mail, or memo; got '{other}'"
            )));
        }
    };
    // Plain mail is read only when mail is what is listed.
    let scan = if wanted_kind == Some("mail") {
        Scan::All
    } else {
        Scan::WorkAndEvents
    };
    let View { records, states } = read_view(store, project_id, identity, scan, false)?;
    let mut items = Vec::new();
    for record in &records {
        let matches_kind = match (wanted_kind, record.kind.as_deref()) {
            (None, Some("need" | "task" | "want" | "chat")) => true,
            (Some("mail"), None) => true,
            (Some(k), Some(rk)) => k == rk,
            _ => false,
        };
        if !matches_kind {
            continue;
        }
        let item_status = item_status(record, &parse_headers(&record.raw_eml), &states);
        if wanted_kind != Some("mail") {
            if status == "open" && item_status != "open" {
                continue;
            }
            if status == "done" && item_status != "done" {
                continue;
            }
        }
        items.push(summary(record, &states));
        if items.len() >= limit {
            break;
        }
    }
    Ok(json!({
        "kind": wanted_kind.unwrap_or("work"),
        "status": status,
        "items": items,
    }))
}

fn show(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let handle = req_str(arguments, "handle")?;
    if let Some(memo) = find_memo(store, project_id, identity, handle)? {
        return Ok(memo);
    }
    let record = lookup_one(store, project_id, identity, handle)?;
    let states = read_view(store, project_id, identity, Scan::WorkAndEvents, false)?.states;
    let rendered = render_message(&record.raw_eml).unwrap_or_default();
    let parsed = to_json_message(&record.message_id, &record.raw_eml).ok();
    Ok(json!({
        "item": summary(&record, &states),
        "text": rendered,
        "letter": parsed,
    }))
}

fn thread(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let handle = req_str(arguments, "handle")?;
    let limit = opt_usize(arguments, "limit", THREAD_DEFAULT, THREAD_MAX)?;
    let seed = lookup_one(store, project_id, identity, handle)?;
    // A thread is made of event letters too, so the view keeps them.
    let View { records, states } = read_view(store, project_id, identity, Scan::All, true)?;
    let seed_letter = parse_letter(&seed.raw_eml);
    let seed_rfc = seed_letter
        .rfc_id
        .clone()
        .unwrap_or_else(|| seed.message_id.clone());
    let mut by_rfc: HashMap<String, &ProjectRecord> = HashMap::new();
    let mut children: HashMap<String, Vec<&ProjectRecord>> = HashMap::new();
    for record in &records {
        let letter = parse_headers(&record.raw_eml);
        if let Some(rfc) = letter.rfc_id.clone() {
            by_rfc.insert(rfc, record);
        }
        if let Some(parent) = letter.in_reply_to.clone().or(letter.event_of.clone()) {
            children.entry(parent).or_default().push(record);
        }
    }
    let mut root_rfc = seed_rfc.clone();
    let mut guard = 0;
    let mut current = Some(
        seed_letter
            .in_reply_to
            .clone()
            .or(seed_letter.event_of.clone()),
    );
    while let Some(Some(parent)) = current {
        if let Some(record) = by_rfc.get(&parent) {
            root_rfc = parent.clone();
            current = Some(parse_headers(&record.raw_eml).in_reply_to);
        } else {
            break;
        }
        guard += 1;
        if guard > 32 {
            break;
        }
    }
    let mut ordered = Vec::new();
    fn walk<'a>(
        rfc: &str,
        children: &HashMap<String, Vec<&'a ProjectRecord>>,
        by_rfc: &HashMap<String, &'a ProjectRecord>,
        ordered: &mut Vec<&'a ProjectRecord>,
        limit: usize,
    ) {
        if ordered.len() >= limit {
            return;
        }
        if let Some(record) = by_rfc.get(rfc) {
            if !ordered
                .iter()
                .any(|seen| seen.message_id == record.message_id)
            {
                ordered.push(*record);
            }
        }
        if let Some(kids) = children.get(rfc) {
            for kid in kids {
                if ordered.len() >= limit {
                    return;
                }
                let letter = parse_headers(&kid.raw_eml);
                if let Some(child_rfc) = letter.rfc_id.as_deref() {
                    walk(child_rfc, children, by_rfc, ordered, limit);
                } else if !ordered.iter().any(|seen| seen.message_id == kid.message_id) {
                    ordered.push(*kid);
                }
            }
        }
    }
    walk(&root_rfc, &children, &by_rfc, &mut ordered, limit);
    if !ordered.iter().any(|r| r.message_id == seed.message_id) {
        ordered.push(&seed);
    }
    let messages: Vec<Value> = ordered
        .into_iter()
        .map(|record| {
            let mut item = summary(record, &states);
            item["body"] = json!(parse_letter(&record.raw_eml).body.trim());
            item
        })
        .collect();
    Ok(json!({
        "handle": handle,
        "state": item_status(&seed, &seed_letter, &states),
        "messages": messages,
    }))
}

fn send(
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
    let to = if kind == "chat" && arguments.get("to").is_none() {
        let prefix = identity.mailspace_prefix().ok_or_else(|| {
            ToolError::new("this identity has no mailspace prefix; it cannot send")
        })?;
        vec![format!("chat@{prefix}.local")]
    } else {
        parse_to(arguments)?
    };
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

fn reply(
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
    let composed = compose_reply(identity, &parent, body).map_err(ToolError::new)?;
    let outcome = append_composed(store, project_id, identity, composed).map_err(map_project)?;
    Ok(json!({
        "message_id": outcome.message_id,
        "handle": handle_of(&outcome.message_id),
        "in_reply_to": handle,
        "sequence": outcome.sequence,
    }))
}

fn settle(
    store: &ProjectStore,
    limiter: &RateLimiter,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
    event: &str,
) -> Result<Value, ToolError> {
    rate_limit(limiter, project_id, &identity.name)?;
    let handle = req_str(arguments, "handle")?;
    let note = opt_str(arguments, "note")?.unwrap_or(event);
    let parent = lookup_one(store, project_id, identity, handle)?;
    let parsed = parse_letter(&parent.raw_eml);
    if parsed.event.is_some() {
        return Err(ToolError::new(format!(
            "{handle} is an event letter, not a need, task, or want"
        )));
    }
    if !matches!(parent.kind.as_deref(), Some("need" | "task" | "want")) {
        return Err(ToolError::new(format!(
            "{handle} is not a need, task, or want"
        )));
    }
    // One clock for the whole call: the fold's future bound and the new
    // event's stamp are read at the same instant.
    let now = chrono::Utc::now().timestamp_micros();
    let states = read_view_at(store, project_id, identity, Scan::WorkAndEvents, false, now)?.states;
    let state = state_of(&parent, &parsed, &states);
    let current = item_status(&parent, &parsed, &states);
    if state.is_some_and(|state| state.absorbed) {
        // A sealed item is final: an event would be ignored everywhere.
        if event == "reopened" {
            return Err(ToolError::new(format!(
                "{handle} is absorbed; an absorbed item is final and cannot be reopened"
            )));
        }
        if current != "done" {
            return Err(ToolError::new(format!(
                "{handle} is absorbed open; an absorbed item is final and cannot be closed"
            )));
        }
    }
    if event == "done" && current == "done" {
        return Ok(json!({
            "handle": handle,
            "mode": "already_done",
        }));
    }
    if event == "reopened" && current != "done" {
        return Ok(json!({
            "handle": handle,
            "mode": "already_open",
        }));
    }
    // Close metadata rides the event letter's front matter; a report reply
    // (below) carries none, and a reopen keeps the facts of its done event.
    let close = close_arguments(arguments)?;
    if event != "done" && close.is_some() {
        return Err(ToolError::new(
            "reopen takes no close metadata: the facts belong to the done event",
        ));
    }
    let caller_prefix = identity.mailspace_prefix();
    let item_domain = parsed.from_domain.as_deref();
    let may_close = match (caller_prefix, item_domain) {
        (Some(prefix), Some(domain)) => domain.eq_ignore_ascii_case(&format!("{prefix}.local")),
        _ => false,
    };
    let composed = if may_close {
        // Stamped like the client: the clock, but never at or below the
        // newest event counted for the item, so a causal successor still wins.
        let at = event_at(now, state.map(|state| state.newest_at));
        let prevs = state
            .map(|state| state.heads.as_slice())
            .unwrap_or_default();
        compose_event(
            identity,
            &parent,
            note,
            event,
            &at,
            prevs,
            close.unwrap_or_default(),
        )
        .map_err(ToolError::new)?
    } else {
        if close.is_some() {
            return Err(ToolError::new(
                "close metadata rides the stamped event letter; this caller files a report reply",
            ));
        }
        compose_reply(identity, &parent, note).map_err(ToolError::new)?
    };
    let outcome = append_composed(store, project_id, identity, composed).map_err(map_project)?;
    let mode = if may_close { "closed" } else { "reported" };
    let mode = if event == "reopened" && may_close {
        "reopened"
    } else if event == "reopened" {
        "reported"
    } else {
        mode
    };
    Ok(json!({
        "handle": handle,
        "mode": mode,
        "event": handle_of(&outcome.message_id),
        "message_id": outcome.message_id,
    }))
}

fn memo(
    store: &ProjectStore,
    limiter: &RateLimiter,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let action = opt_str(arguments, "action")?.unwrap_or("list");
    match action {
        "list" => Ok(json!({ "items": list_memos(store, project_id, identity)? })),
        "show" => {
            let handle = req_str(arguments, "handle")?;
            find_memo(store, project_id, identity, handle)?
                .ok_or_else(|| ToolError::new(format!("memo not found: {handle}")))
        }
        "save" => {
            rate_limit(limiter, project_id, &identity.name)?;
            let subject = req_str(arguments, "subject")?;
            let body = req_str(arguments, "body")?;
            if subject.trim().is_empty() || body.trim().is_empty() {
                return Err(ToolError::new("subject and body must be non-empty"));
            }
            // Tags ride the memo's front matter; a body that opens with `+++`
            // is the whole letter, and tags beside it are refused, as the
            // send tool refuses sugar beside a whole letter.
            let tags = string_list(arguments, "tags")?;
            if !tags.is_empty() && opens_with_fence(body) {
                return Err(ToolError::new(
                    "the body is a whole letter (it opens with +++): put tags in its front matter instead of passing tags",
                ));
            }
            save_memo(store, project_id, identity, subject, body, tags)
        }
        other => Err(ToolError::new(format!(
            "memo action must be list, show, or save; got '{other}'"
        ))),
    }
}

fn graph(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let view = opt_str(arguments, "view")?.unwrap_or("status");
    if !matches!(view, "ready" | "status") {
        return Err(ToolError::new("view must be ready or status"));
    }
    let View { records, states } = read_view(store, project_id, identity, Scan::All, false)?;
    let by_handle = handle_index(&records);
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    let mut done = Vec::new();
    for record in &records {
        if record.kind.is_none() {
            continue;
        }
        let letter = parse_headers(&record.raw_eml);
        let status = item_status(record, &letter, &states);
        let item = summary(record, &states);
        if status == "done" {
            done.push(item);
            continue;
        }
        let open_deps = open_dependencies(&depends_of(record), &by_handle, &states);
        if open_deps.is_empty() {
            ready.push(item);
        } else {
            let mut blocked_item = item;
            blocked_item["blocked_by"] = json!(open_deps);
            blocked.push(blocked_item);
        }
    }
    if view == "ready" {
        return Ok(json!({ "ready": ready, "blocked": blocked }));
    }
    Ok(json!({
        "ready": ready,
        "blocked": blocked,
        "done": done,
        "totals": {
            "ready": ready.len(),
            "blocked": blocked.len(),
            "done": done.len(),
        },
    }))
}

/// What a tool reads of a project as the caller: the letters it may see, in
/// append order, and the item states folded from every event letter it may
/// see.
struct View {
    /// The letters the scan returned, event letters left out unless the
    /// read asked to keep them.
    records: Vec<ProjectRecord>,
    states: HashMap<String, ItemState>,
}

/// [`read_view_at`] at the clock now.
fn read_view(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    scan: Scan,
    keep_events: bool,
) -> Result<View, ToolError> {
    let now = chrono::Utc::now().timestamp_micros();
    read_view_at(store, project_id, identity, scan, keep_events, now)
}

/// Walk every letter the caller may see, past the first page and the last,
/// folding the events as they go and keeping only what a caller needs: an
/// event letter is dropped once folded unless `keep_events`, so memory holds
/// the work and mail the tool lists and never the events that fold into it.
/// `now` is the fold's clock for the future bound.
fn read_view_at(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    scan: Scan,
    keep_events: bool,
    now: i64,
) -> Result<View, ToolError> {
    let mut fold = EventFold::new(now);
    let mut records = Vec::new();
    store
        .scan_visible(project_id, &Viewer::of(identity), scan, |record| {
            let is_event = fold.observe(&record);
            if keep_events || !is_event {
                records.push(record);
            }
        })
        .map_err(map_project)?;
    Ok(View {
        records,
        states: fold.finish(),
    })
}

fn lookup_one(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    handle: &str,
) -> Result<ProjectRecord, ToolError> {
    if !valid_handle(handle) {
        return Err(ToolError::new("handle must be 8 hex characters"));
    }
    let found = store
        .lookup_handles(project_id, &[handle.to_owned()], &Viewer::of(identity))
        .map_err(map_project)?;
    found
        .records
        .into_iter()
        .next()
        .ok_or_else(|| ToolError::new(format!("not found: {handle}")))
}

fn summary(record: &ProjectRecord, states: &HashMap<String, ItemState>) -> Value {
    let letter = parse_headers(&record.raw_eml);
    let mut item = json!({
        "handle": handle_of(&record.message_id),
        "message_id": record.message_id,
        "kind": if letter.event.is_some() { "event" } else { record.kind.as_deref().unwrap_or("mail") },
        "from": record.from,
        "to": record.to,
        "subject": letter.subject,
        "status": item_status(record, &letter, states),
        "appended_at": record.appended_at,
        "depends_on": depends_of(record),
    });
    if let Some(event) = &letter.event {
        item["event"] = json!(event);
    } else if let Some(state) = state_of(record, &letter, states) {
        if !state.marks.is_empty() {
            item["marks"] = json!(state.marks);
        }
        if !state.units.is_empty() {
            item["units"] = json!(state.units);
        }
        if state.absorbed {
            item["absorbed"] = json!(true);
        }
    }
    item
}

fn item_is_open(record: &ProjectRecord, states: &HashMap<String, ItemState>) -> bool {
    item_status(record, &parse_headers(&record.raw_eml), states) == "open"
}

/// The first record of each handle, by lowercase handle: what a dependency
/// names, found without a scan per dependency.
fn handle_index(records: &[ProjectRecord]) -> HashMap<String, &ProjectRecord> {
    let mut index = HashMap::with_capacity(records.len());
    for record in records {
        if let Some(handle) = handle_of(&record.message_id) {
            index.entry(handle.to_ascii_lowercase()).or_insert(record);
        }
    }
    index
}

fn ready_handles(records: &[ProjectRecord], states: &HashMap<String, ItemState>) -> Vec<String> {
    let by_handle = handle_index(records);
    records
        .iter()
        .filter(|record| record.kind.is_some() && item_is_open(record, states))
        .filter(|record| open_dependencies(&depends_of(record), &by_handle, states).is_empty())
        .filter_map(|record| handle_of(&record.message_id).map(str::to_owned))
        .collect()
}

/// `depends_on` from the letter's front matter. A legacy letter (no
/// `X-Vivi-Format`) and a letter that claims a format but does not parse
/// contribute none, and neither fails the graph: stored history stays readable.
fn depends_of(record: &ProjectRecord) -> Vec<String> {
    let handle = handle_of(&record.message_id).unwrap_or("");
    let Ok(letter) = of_message(&record.raw_eml, handle) else {
        return Vec::new();
    };
    match letter.front {
        Front::Need(front) => front.depends_on,
        Front::Want(front) => front.depends_on,
        Front::Task(front) => front.depends_on,
        _ => Vec::new(),
    }
}

fn open_dependencies(
    depends_on: &[String],
    by_handle: &HashMap<String, &ProjectRecord>,
    states: &HashMap<String, ItemState>,
) -> Vec<String> {
    depends_on
        .iter()
        .filter(|dep| match by_handle.get(&dep.to_ascii_lowercase()) {
            Some(dep_record) => item_is_open(dep_record, states),
            None => true,
        })
        .cloned()
        .collect()
}

/// The `X-Vivi-Event-At` of a new event: the clock, or one microsecond past
/// the newest counted event of the item, whichever is later. A writer never
/// stamps an event older than one it has seen. The stamp is RFC 3339 UTC in
/// microseconds, as long as a reader parses it back: past the year 9999 the
/// year takes five digits, so the clock is stamped instead.
fn event_at(now_micros: i64, newest: Option<i64>) -> String {
    let floor = newest.map_or(now_micros, |at| at.saturating_add(1));
    let stamp = stamped(now_micros.max(floor));
    if chrono::DateTime::parse_from_rfc3339(&stamp).is_ok() {
        return stamp;
    }
    stamped(chrono::Utc::now().timestamp_micros())
}

/// An instant in Unix microseconds as RFC 3339 UTC with microseconds; the
/// clock when the instant is outside what `chrono` can hold.
fn stamped(micros: i64) -> String {
    chrono::DateTime::from_timestamp_micros(micros)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn open_mailspace(store: &ProjectStore, project_id: &str) -> Result<Storage, ToolError> {
    let dir = store.mailspace_dir(project_id).map_err(map_project)?;
    Storage::open_mailspace(&dir).map_err(|e| ToolError::new(e.to_string()))
}

fn list_memos(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
) -> Result<Vec<Value>, ToolError> {
    let storage = open_mailspace(store, project_id)?;
    let rows = storage
        .list_messages_by_account_role(&identity.name, "memos")
        .map_err(|e| ToolError::new(e.to_string()))?;
    let mut items = Vec::new();
    for row in rows {
        let raw = storage
            .read_blob(&row.content_id)
            .map_err(|e| ToolError::new(e.to_string()))?;
        let letter = parse_letter(&raw);
        items.push(json!({
            "handle": handle_of(&row.message_id).unwrap_or(row.message_id.as_str()),
            "message_id": row.message_id,
            "subject": letter.subject,
            "body": letter.body.trim(),
            "from": letter.from,
        }));
    }
    Ok(items)
}

fn find_memo(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    handle: &str,
) -> Result<Option<Value>, ToolError> {
    Ok(list_memos(store, project_id, identity)?
        .into_iter()
        .find(|item| {
            item["handle"].as_str() == Some(handle) || item["message_id"].as_str() == Some(handle)
        }))
}

fn save_memo(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    subject: &str,
    body: &str,
    tags: Vec<String>,
) -> Result<Value, ToolError> {
    let from = caller_address(identity).map_err(ToolError::new)?;
    // A memo is a letter: its tags ride `MemoFront` front matter (bare prose
    // when there are none) and the composer's gate writes the format header.
    let tags = normalized_memo_tags(&tags).map_err(ToolError::new)?;
    let body = render_front(&MemoFront { tags }, body)
        .map_err(|error| ToolError::new(format!("memo letter: {error}")))?;
    let composed = compose_send(
        identity,
        std::slice::from_ref(&from),
        subject,
        &body,
        "memo",
    )
    .map_err(ToolError::new)?;
    let traveling_id = composed.traveling_id.clone();
    let storage = open_mailspace(store, project_id)?;
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: identity.name.clone(),
                local_role: "memos".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: Some(composed.traveling_id),
                seed_hint: String::new(),
                remote: None,
            },
            &composed.raw_eml,
        )
        .map_err(|e| ToolError::new(e.to_string()))?;
    Ok(json!({
        "handle": handle_of(&traveling_id),
        "message_id": traveling_id,
        "subject": subject,
    }))
}

pub(super) fn parse_to(arguments: &Value) -> Result<Vec<String>, ToolError> {
    match arguments.get("to") {
        Some(Value::String(raw)) => {
            let list: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            if list.is_empty() {
                Err(ToolError::new("to must name at least one address"))
            } else {
                Ok(list)
            }
        }
        Some(Value::Array(items)) => {
            let list: Result<Vec<_>, _> = items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| ToolError::new("to must be strings"))
                })
                .collect();
            let list = list?;
            if list.is_empty() {
                Err(ToolError::new("to must name at least one address"))
            } else {
                Ok(list)
            }
        }
        _ => Err(ToolError::new("to is required")),
    }
}

pub(super) fn parse_depends(arguments: &Value) -> Result<Vec<String>, ToolError> {
    match arguments.get("depends_on") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(raw)) => {
            let handle = raw.trim();
            if handle.is_empty() {
                return Ok(Vec::new());
            }
            if !valid_handle(handle) {
                return Err(ToolError::new("depends_on entries must be 8 hex handles"));
            }
            Ok(vec![handle.to_owned()])
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                let handle = item
                    .as_str()
                    .ok_or_else(|| ToolError::new("depends_on entries must be strings"))?;
                if !valid_handle(handle) {
                    return Err(ToolError::new("depends_on entries must be 8 hex handles"));
                }
                Ok(handle.to_owned())
            })
            .collect(),
        _ => Err(ToolError::new(
            "depends_on must be a handle or a list of handles",
        )),
    }
}

/// The sugar arguments of a `send`, as the CLI's flags: the front matter a
/// need, task, or want is built from. Every field is empty when its
/// argument is absent.
pub(super) fn parse_sugar(arguments: &Value) -> Result<crate::letter::sugar::Sugar, ToolError> {
    Ok(crate::letter::sugar::Sugar {
        done_when: sugar_list(arguments, "done_when")?,
        target: opt_str(arguments, "target")?.map(str::to_owned),
        write_scope: sugar_list(arguments, "write_scope")?,
        serves: opt_str(arguments, "serves")?.map(str::to_owned),
        explores: opt_str(arguments, "explores")?.map(str::to_owned),
        question: opt_str(arguments, "question")?.map(str::to_owned),
        depends_on: parse_depends(arguments)?,
    })
}

/// A string-or-list sugar argument as a list; blank entries are refused.
fn sugar_list(arguments: &Value, name: &str) -> Result<Vec<String>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![trimmed.to_owned()])
            }
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                let text = item
                    .as_str()
                    .map(str::trim)
                    .ok_or_else(|| ToolError::new(format!("{name} entries must be strings")))?;
                if text.is_empty() {
                    return Err(ToolError::new(format!("{name} entries must not be blank")));
                }
                Ok(text.to_owned())
            })
            .collect(),
        _ => Err(ToolError::new(format!(
            "{name} must be a string or a list of strings"
        ))),
    }
}

/// The sugar arguments `sugar` carries, by name, in schema order: what a
/// refusal of a sugarless kind names.
fn sugar_given(sugar: &crate::letter::sugar::Sugar) -> Vec<&'static str> {
    [
        (!sugar.done_when.is_empty(), "done_when"),
        (sugar.target.is_some(), "target"),
        (!sugar.write_scope.is_empty(), "write_scope"),
        (sugar.serves.is_some(), "serves"),
        (sugar.explores.is_some(), "explores"),
        (sugar.question.is_some(), "question"),
        (!sugar.depends_on.is_empty(), "depends_on"),
    ]
    .into_iter()
    .filter(|(present, _)| *present)
    .map(|(_, name)| name)
    .collect()
}

/// The body a `send` sends. For a need, task, or want the sugar arguments
/// build its front matter, exactly as the CLI's flags do; `apply` speaks
/// for every refusal (a need without `done_when`, a want without
/// `question`, or a sugar argument beside a body that already opens with
/// `+++`, the whole letter). Any other kind takes no sugar argument, and
/// its prose passes as written.
pub(super) fn sugared_body(kind: &str, body: &str, arguments: &Value) -> Result<String, ToolError> {
    let sugar = parse_sugar(arguments)?;
    if matches!(kind, "need" | "task" | "want") {
        let sugared = crate::letter::sugar::apply(kind, body, &sugar)
            .map_err(|error| ToolError::new(error.to_string()))?;
        return Ok(sugared.body);
    }
    let given = sugar_given(&sugar);
    if given.is_empty() {
        Ok(body.to_owned())
    } else {
        Err(ToolError::new(format!(
            "{kind} takes no sugar arguments: {}",
            given.join(", ")
        )))
    }
}

/// The close metadata a `done` call passes: `verdict`, `branch`, `commit`
/// and a `repos` object of repo name to tip, exactly the front matter the
/// event letter carries. `Ok(None)` when the call passes none. The front's
/// own rules (branch and commit together, no blank fact) are checked when
/// the letter is composed.
pub(super) fn close_arguments(arguments: &Value) -> Result<Option<CloseFront>, ToolError> {
    if ["verdict", "branch", "commit", "repos"]
        .iter()
        .all(|key| matches!(arguments.get(*key), None | Some(Value::Null)))
    {
        return Ok(None);
    }
    let repos = match arguments.get("repos") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(repo, tip)| {
                tip.as_str()
                    .map(|tip| (repo.clone(), tip.to_owned()))
                    .ok_or_else(|| ToolError::new("repos must map repo names to tip commits"))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?,
        _ => {
            return Err(ToolError::new(
                "repos must be an object of repo name to tip commit",
            ));
        }
    };
    Ok(Some(CloseFront {
        verdict: opt_str(arguments, "verdict")?.map(str::to_owned),
        branch: opt_str(arguments, "branch")?.map(str::to_owned),
        commit: opt_str(arguments, "commit")?.map(str::to_owned),
        repos,
    }))
}

/// Trim memo tags and refuse ones that are empty or contain whitespace, so
/// every stored tag is a single token (vivi's own rule for memo tags).
fn normalized_memo_tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut normalized = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || tag.chars().any(char::is_whitespace) {
            return Err(format!(
                "memo tag {tag:?} must be non-empty and contain no whitespace"
            ));
        }
        normalized.push(tag.to_owned());
    }
    Ok(normalized)
}

pub(super) fn req_str<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::new(format!("{name} is required")))
}

pub(super) fn opt_str<'a>(arguments: &'a Value, name: &str) -> Result<Option<&'a str>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            Ok((!trimmed.is_empty()).then_some(trimmed))
        }
        _ => Err(ToolError::new(format!("{name} must be a string"))),
    }
}

pub(super) fn opt_usize(
    arguments: &Value,
    name: &str,
    default: usize,
    max: usize,
) -> Result<usize, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => {
            let parsed = n
                .as_u64()
                .ok_or_else(|| ToolError::new(format!("{name} must be a positive integer")))?;
            if parsed < 1 {
                return Err(ToolError::new(format!("{name} must be a positive integer")));
            }
            Ok((parsed as usize).min(max))
        }
        _ => Err(ToolError::new(format!("{name} must be a positive integer"))),
    }
}

pub(super) fn rate_limit(
    limiter: &RateLimiter,
    project_id: &str,
    name: &str,
) -> Result<(), ToolError> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    if limiter.allow_send(project_id, hasher.finish() as u32) {
        Ok(())
    } else {
        Err(ToolError::new(format!(
            "over {} writes/min/identity",
            limiter.limit().unwrap_or(0)
        )))
    }
}

pub(super) fn map_project(err: ProjectError) -> ToolError {
    match err {
        ProjectError::BadRequest(message) => ToolError::new(message),
        ProjectError::LetterFormat(message) => ToolError::new(message),
        ProjectError::TooLarge => ToolError::new("letter over 64 KiB"),
        ProjectError::Conflict(message) => ToolError::new(message),
        ProjectError::ProjectNotFound => ToolError::new("unknown project"),
        ProjectError::InvalidProjectId => ToolError::new("invalid project id"),
        ProjectError::Storage(message) => {
            tracing::error!("mcp project storage failure: {message}");
            ToolError::new("storage failure")
        }
        ProjectError::ReadOnly(message) => ToolError::new(message),
        ProjectError::Gone { closed: true } => ToolError::new("this chat is closed"),
        ProjectError::Gone { closed: false } => ToolError::new("this chat has expired"),
    }
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tests;
