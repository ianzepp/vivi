//! Curated MCP tools. Reads use the transport index (visibility) plus
//! vivi's mailspace store for memos. Writes compose through
//! `vivi::message::build_compose_draft` and append as the authenticated
//! caller — the server mints the traveling id (decided item 15).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use serde_json::{Value, json};
use vivi::message::{render_message, to_json_message};
use vivi::storage::{MessageIngestRequest, Storage};

use crate::identities::IdentityRecord;
use crate::projects::{ListQuery, ProjectError, ProjectRecord, ProjectStore, Viewer};
use crate::safety::RateLimiter;

use super::letters::{
    ItemState, ParsedLetter, append_composed, caller_address, compose_event, compose_reply,
    compose_send, fold_item_states, handle_of, item_status, parse_letter, state_of, valid_handle,
};

const LIST_DEFAULT: usize = 50;
const LIST_MAX: usize = 100;
const THREAD_DEFAULT: usize = 20;
const THREAD_MAX: usize = 50;

pub struct ToolError {
    pub message: String,
}

impl ToolError {
    fn new(message: impl Into<String>) -> Self {
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
        other => Err(ToolError::new(format!("unknown tool '{other}'"))),
    }
}

fn overview(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
) -> Result<Value, ToolError> {
    let all = visible_all(store, project_id, identity)?;
    let states = fold_item_states(&all);
    // Event letters are not work and not mail: they only move item state.
    let records = without_events(&all);
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
    let ready = ready_handles(&records, &states);
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
            "ready": ready.len(),
        },
        "ready": ready,
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
    let all = visible_all(store, project_id, identity)?;
    let states = fold_item_states(&all);
    let records = without_events(&all);
    let wanted_kind = match kind.as_deref() {
        None => None,
        Some("need" | "task" | "want" | "mail" | "chat") => kind.as_deref(),
        Some(other) => {
            return Err(ToolError::new(format!(
                "kind must be need, task, want, chat, mail, or memo; got '{other}'"
            )));
        }
    };
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
        let item_status = item_status(record, &parse_letter(&record.raw_eml), &states);
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
    let states = fold_item_states(&visible_all(store, project_id, identity)?);
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
    let records = visible_all(store, project_id, identity)?;
    let states = fold_item_states(&records);
    let seed_letter = parse_letter(&seed.raw_eml);
    let seed_rfc = seed_letter
        .rfc_id
        .clone()
        .unwrap_or_else(|| seed.message_id.clone());
    let mut by_rfc: HashMap<String, &ProjectRecord> = HashMap::new();
    let mut children: HashMap<String, Vec<&ProjectRecord>> = HashMap::new();
    for record in &records {
        let letter = parse_letter(&record.raw_eml);
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
            current = Some(parse_letter(&record.raw_eml).in_reply_to);
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
                let letter = parse_letter(&kid.raw_eml);
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
    let depends_on = parse_depends(arguments)?;
    let composed =
        compose_send(identity, &to, subject, body, kind, &depends_on).map_err(ToolError::new)?;
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
    let records = visible_all(store, project_id, identity)?;
    let states = fold_item_states(&records);
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
    let caller_prefix = identity.mailspace_prefix();
    let item_domain = parsed.from_domain.as_deref();
    let may_close = match (caller_prefix, item_domain) {
        (Some(prefix), Some(domain)) => domain.eq_ignore_ascii_case(&format!("{prefix}.local")),
        _ => false,
    };
    let composed = if may_close {
        // Stamped like the client: the clock, but never at or below the
        // newest event held for the item, so a causal successor still wins.
        let at = event_at(
            chrono::Utc::now().timestamp_micros(),
            state.map(|state| state.newest_at),
        );
        let prevs = state
            .map(|state| state.heads.as_slice())
            .unwrap_or_default();
        compose_event(identity, &parent, note, event, &at, prevs).map_err(ToolError::new)?
    } else {
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
            save_memo(store, project_id, identity, subject, body)
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
    let all = visible_all(store, project_id, identity)?;
    let states = fold_item_states(&all);
    let records = without_events(&all);
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    let mut done = Vec::new();
    for record in &records {
        if record.kind.is_none() {
            continue;
        }
        let letter = parse_letter(&record.raw_eml);
        let status = item_status(record, &letter, &states);
        let item = summary(record, &states);
        if status == "done" {
            done.push(item);
            continue;
        }
        let open_deps = open_dependencies(&letter, &records, &states);
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

fn visible_all(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
) -> Result<Vec<ProjectRecord>, ToolError> {
    let page = store
        .read_page(project_id, &ListQuery::since(0), &Viewer::of(identity))
        .map_err(map_project)?;
    Ok(page.records)
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
    let letter = parse_letter(&record.raw_eml);
    let mut item = json!({
        "handle": handle_of(&record.message_id),
        "message_id": record.message_id,
        "kind": if letter.event.is_some() { "event" } else { record.kind.as_deref().unwrap_or("mail") },
        "from": record.from,
        "to": record.to,
        "subject": letter.subject,
        "status": item_status(record, &letter, states),
        "appended_at": record.appended_at,
        "depends_on": letter.depends_on,
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

/// The records that are not event letters: an `X-Vivi-Event` letter moves
/// the state of the item it names and is never itself an item to list,
/// count, route, or settle, whatever envelope kind it travels under.
fn without_events(records: &[ProjectRecord]) -> Vec<&ProjectRecord> {
    records
        .iter()
        .filter(|record| parse_letter(&record.raw_eml).event.is_none())
        .collect()
}

fn item_is_open(record: &ProjectRecord, states: &HashMap<String, ItemState>) -> bool {
    item_status(record, &parse_letter(&record.raw_eml), states) == "open"
}

fn ready_handles(records: &[&ProjectRecord], states: &HashMap<String, ItemState>) -> Vec<String> {
    records
        .iter()
        .filter(|record| record.kind.is_some() && item_is_open(record, states))
        .filter(|record| {
            let letter = parse_letter(&record.raw_eml);
            open_dependencies(&letter, records, states).is_empty()
        })
        .filter_map(|record| handle_of(&record.message_id).map(str::to_owned))
        .collect()
}

fn open_dependencies(
    letter: &ParsedLetter,
    records: &[&ProjectRecord],
    states: &HashMap<String, ItemState>,
) -> Vec<String> {
    letter
        .depends_on
        .iter()
        .filter(|dep| {
            let handle = dep.to_ascii_lowercase();
            let Some(dep_record) = records.iter().find(|r| {
                handle_of(&r.message_id).is_some_and(|h| h.eq_ignore_ascii_case(&handle))
            }) else {
                return true;
            };
            item_is_open(dep_record, states)
        })
        .cloned()
        .collect()
}

/// The `X-Vivi-Event-At` of a new event: the clock, or one microsecond past
/// the newest event held for the item, whichever is later. A writer never
/// stamps an event older than one it has seen.
fn event_at(now_micros: i64, newest: Option<i64>) -> String {
    let floor = newest.map_or(now_micros, |at| at.saturating_add(1));
    chrono::DateTime::from_timestamp_micros(now_micros.max(floor))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn open_mailspace(store: &ProjectStore, project_id: &str) -> Result<Storage, ToolError> {
    let dir = store
        .existing_project_dir(project_id)
        .map_err(map_project)?;
    Storage::open_mailspace(&dir.join(".vivi")).map_err(|e| ToolError::new(e.to_string()))
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
) -> Result<Value, ToolError> {
    let from = caller_address(identity).map_err(ToolError::new)?;
    let composed = compose_send(
        identity,
        std::slice::from_ref(&from),
        subject,
        body,
        "memo",
        &[],
    )
    .map_err(ToolError::new)?;
    let traveling_id = composed.traveling_id.clone();
    let mut storage = open_mailspace(store, project_id)?;
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

fn parse_to(arguments: &Value) -> Result<Vec<String>, ToolError> {
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

fn parse_depends(arguments: &Value) -> Result<Vec<String>, ToolError> {
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

fn req_str<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::new(format!("{name} is required")))
}

fn opt_str<'a>(arguments: &'a Value, name: &str) -> Result<Option<&'a str>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            Ok((!trimmed.is_empty()).then_some(trimmed))
        }
        _ => Err(ToolError::new(format!("{name} must be a string"))),
    }
}

fn opt_usize(
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

fn rate_limit(limiter: &RateLimiter, project_id: &str, name: &str) -> Result<(), ToolError> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    if limiter.allow_send(project_id, hasher.finish() as u32) {
        Ok(())
    } else {
        Err(ToolError::new("over 30 writes/min/identity"))
    }
}

fn map_project(err: ProjectError) -> ToolError {
    match err {
        ProjectError::BadRequest(message) => ToolError::new(message),
        ProjectError::TooLarge => ToolError::new("letter over 64 KiB"),
        ProjectError::Conflict(message) => ToolError::new(message),
        ProjectError::ProjectNotFound => ToolError::new("unknown project"),
        ProjectError::InvalidProjectId => ToolError::new("invalid project id"),
        ProjectError::Storage(message) => {
            tracing::error!("mcp project storage failure: {message}");
            ToolError::new("storage failure")
        }
        ProjectError::Gone { closed: true } => ToolError::new("this chat is closed"),
        ProjectError::Gone { closed: false } => ToolError::new("this chat has expired"),
    }
}
