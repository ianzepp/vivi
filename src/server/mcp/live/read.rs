//! Hosted-project reads: live queries of the mailspace through the library.

use std::collections::HashSet;

use serde_json::{Value, json};
use vivi::mailspace::board::BoardOptions;
use vivi::mailspace::{
    folded_marks, frontier_from_show, is_foreign_account, is_gate_kind, marks_include,
    prefix_of_domain, validate_marks_filter,
};
use vivi::message::{render_message, to_json_message};
use vivi::storage::{MailspaceEvent, StoredMessageView};

use crate::mcp::letters::parse_letter;
use crate::mcp::tools::{OVERVIEW_READY_CAP, ToolError, opt_str, opt_usize, req_str};

use super::{Hosted, Located, is_work, lib_error, locate, status_for_role};

const LIST_DEFAULT: usize = 50;
const LIST_MAX: usize = 100;
const THREAD_DEFAULT: usize = 20;
const THREAD_MAX: usize = 50;
/// The wants the CLI's `board` shows per identity unless asked otherwise.
const BOARD_WANTS_DEFAULT: usize = 5;
const RECENT: usize = 8;
/// Open tasks and needs shown per role unless asked otherwise.
const BOARD_ITEMS_DEFAULT: usize = 5;
/// Graph nodes listed per frontier view (ready, blocked, active).
const BOARD_GRAPH_DEFAULT: usize = 25;
/// Characters of a last-event note the board keeps unless `notes` is set.
const NOTE_PREVIEW_CHARS: usize = 80;

/// `marks` and friends: one slug or a list, as a string or an array.
pub(super) fn string_list(arguments: &Value, name: &str) -> Result<Vec<String>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(raw)) => Ok(raw
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(|s| s.trim().to_owned())
                    .ok_or_else(|| ToolError::new(format!("{name} must be strings")))
            })
            .collect(),
        _ => Err(ToolError::new(format!(
            "{name} must be a string or a list of strings"
        ))),
    }
}

fn event_json(event: &MailspaceEvent) -> Value {
    json!({
        "occurred_at": event.occurred_at,
        "command": event.command,
        "event_type": event.event_type,
        "actor_identity": event.actor_identity,
        "note": event.note,
    })
}

/// One item as a tool lists it: today's `summary` keys where the mailspace
/// has them, plus the folder (`role`), folded `marks` and the last event.
pub(super) fn item_row(
    kind: &str,
    view: &StoredMessageView,
    events: &[MailspaceEvent],
    depends_on: Option<Vec<String>>,
) -> Value {
    let mut item = json!({
        "handle": view.handle,
        "message_id": view.message_id,
        "kind": kind,
        "from": view.from_addr,
        "to": view.to_addr,
        "subject": view.subject,
        "role": view.local_role,
        "date": view.date,
        "marks": folded_marks(events),
        "last_event": events.last().map(event_json),
    });
    if is_work(kind) {
        item["status"] = json!(status_for_role(&view.local_role));
    }
    if let Some(depends_on) = depends_on {
        item["depends_on"] = json!(depends_on);
    }
    if is_foreign_account(&view.account) {
        item["held_for"] = json!(view.account);
    }
    if view.absorbed_at.is_some() {
        item["absorbed"] = json!(true);
    }
    item
}

/// A `from` / `to` filter as the CLI resolves it: a role name matches its
/// current and legacy addresses, anything else matches as a substring.
fn needles(h: &Hosted, raw: Option<&str>) -> Option<Vec<String>> {
    let raw = raw?;
    let found = match h.ms.resolve_identity(raw) {
        Ok(name) => vec![
            h.ms.address_for(&name),
            format!("{name}@{}", h.ms.legacy_domain()),
        ],
        Err(_) => vec![raw.to_owned()],
    };
    Some(found.into_iter().map(|n| n.to_ascii_lowercase()).collect())
}

fn headers_match(view: &StoredMessageView, from: Option<&[String]>, to: Option<&[String]>) -> bool {
    let hit = |needles: &[String], hay: &str| {
        let hay = hay.to_ascii_lowercase();
        needles.iter().any(|n| hay.contains(n.as_str()))
    };
    from.is_none_or(|n| hit(n, &view.from_addr))
        && to.is_none_or(|n| hit(n, &view.to_addr) || hit(n, &view.cc_addr))
}

fn open_role(kind: &str) -> &'static str {
    match kind {
        "need" => "needs",
        "task" => "tasks",
        _ => "wants",
    }
}

/// Whose folders a listing reads: the acting identity unless `for` names
/// another role, or `all` for every role of the mailspace (what the CLI
/// lists without `--for`).
fn list_scope(h: &Hosted, arguments: &Value) -> Result<Option<String>, ToolError> {
    match opt_str(arguments, "for")? {
        None => Ok(Some(h.actor.clone())),
        Some("all") => Ok(None),
        Some(other) => h.ms.resolve_identity(other).map(Some).map_err(lib_error),
    }
}

/// The identity's work of `kind` in the folders `status` names, the way the
/// CLI's `need|task|want list --for <identity>` reads them.
fn work_views(
    h: &Hosted,
    scope: Option<&str>,
    kind: &str,
    status: &str,
    from: Option<&[String]>,
    to: Option<&[String]>,
) -> Result<Vec<StoredMessageView>, ToolError> {
    let open = open_role(kind);
    let roles: Vec<&str> = match status {
        "done" => vec!["done"],
        "all" => vec![open, "sent", "done"],
        _ => vec![open, "sent"],
    };
    let actor = scope;
    let mut views = Vec::new();
    for role in &roles {
        let listed = if *role == "sent" {
            h.ms.list_outbound_work(actor, kind).map_err(lib_error)?
        } else {
            let mut listed = h.ms.list_kind(actor, role, kind).map_err(lib_error)?;
            listed.extend(
                h.ms.assigned_work_for_role(actor, role, kind)
                    .map_err(lib_error)?,
            );
            listed
        };
        views.extend(listed.into_iter().filter(|view| {
            (roles.contains(&"done") || view.absorbed_at.is_none()) && headers_match(view, from, to)
        }));
    }
    Ok(views)
}

pub(super) fn list(h: &Hosted, arguments: &Value) -> Result<Value, ToolError> {
    let kind = opt_str(arguments, "kind")?;
    let status = opt_str(arguments, "status")?.unwrap_or("open");
    if !matches!(status, "open" | "done" | "all") {
        return Err(ToolError::new("status must be open, done, or all"));
    }
    let limit = opt_usize(arguments, "limit", LIST_DEFAULT, LIST_MAX)?;
    let marks = string_list(arguments, "marks")?;
    validate_marks_filter(&marks).map_err(lib_error)?;
    let from = needles(h, opt_str(arguments, "from")?);
    let to = needles(h, opt_str(arguments, "to")?);
    let scope = list_scope(h, arguments)?;
    let scope = scope.as_deref();
    match kind {
        Some("memo") => {
            let mut memos = memo_items(h, scope)?;
            memos.truncate(limit);
            Ok(json!({ "kind": "memo", "items": memos }))
        }
        Some("chat") => Err(ToolError::new(
            "chat is not a kind of a hosted project; list mail instead",
        )),
        Some("mail") => list_mail(h, scope, from.as_deref(), to.as_deref(), &marks, limit),
        Some(k @ ("need" | "task" | "want")) => list_work(
            h,
            scope,
            &[k],
            k,
            status,
            from.as_deref(),
            to.as_deref(),
            &marks,
            limit,
        ),
        None => list_work(
            h,
            scope,
            &["need", "task", "want"],
            "work",
            status,
            from.as_deref(),
            to.as_deref(),
            &marks,
            limit,
        ),
        Some(other) => Err(ToolError::new(format!(
            "kind must be need, task, want, mail, or memo; got '{other}'"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn list_work(
    h: &Hosted,
    scope: Option<&str>,
    kinds: &[&str],
    label: &str,
    status: &str,
    from: Option<&[String]>,
    to: Option<&[String]>,
    marks: &[String],
    limit: usize,
) -> Result<Value, ToolError> {
    let storage = h.ms.storage().map_err(lib_error)?;
    let mut rows: Vec<(&str, StoredMessageView)> = Vec::new();
    for kind in kinds {
        for view in work_views(h, scope, kind, status, from, to)? {
            rows.push((kind, view));
        }
    }
    let ids: Vec<String> = rows.iter().map(|(_, v)| v.message_id.clone()).collect();
    let events = storage
        .list_mailspace_events_for_messages(&ids)
        .map_err(lib_error)?;
    let mut items = Vec::new();
    for (kind, view) in &rows {
        let log = events.get(&view.message_id).map_or(&[][..], Vec::as_slice);
        if !marks_include(&folded_marks(log), marks) {
            continue;
        }
        let deps = h.ms.task_depends_on(&view.message_id).map_err(lib_error)?;
        items.push(item_row(kind, view, log, Some(deps)));
        if items.len() >= limit {
            break;
        }
    }
    Ok(json!({ "kind": label, "status": status, "items": items }))
}

fn list_mail(
    h: &Hosted,
    scope: Option<&str>,
    from: Option<&[String]>,
    to: Option<&[String]>,
    marks: &[String],
    limit: usize,
) -> Result<Value, ToolError> {
    let views = match scope {
        Some(identity) => h.ms.list(identity, "inbox").map_err(lib_error)?,
        None => {
            h.ms.storage()
                .map_err(lib_error)?
                .list_messages_by_role("inbox")
                .map_err(lib_error)?
        }
    };
    let storage = h.ms.storage().map_err(lib_error)?;
    let mut items = Vec::new();
    for view in views {
        if view.absorbed_at.is_some() || !headers_match(&view, from, to) {
            continue;
        }
        let log = storage
            .list_mailspace_events(&view.message_id)
            .map_err(lib_error)?;
        if !marks_include(&folded_marks(&log), marks) {
            continue;
        }
        items.push(item_row("mail", &view, &log, None));
        if items.len() >= limit {
            break;
        }
    }
    Ok(json!({ "kind": "mail", "status": "all", "items": items }))
}

pub(super) fn memo_json(view: &StoredMessageView, raw: &[u8]) -> Value {
    let letter = parse_letter(raw);
    json!({
        "handle": view.handle,
        "message_id": view.message_id,
        "subject": letter.subject,
        "body": letter.body.trim(),
        "from": letter.from,
    })
}

/// The acting identity's memos, as `vivi memo list --for <identity>` reads
/// them.
pub(super) fn memo_items(h: &Hosted, scope: Option<&str>) -> Result<Vec<Value>, ToolError> {
    let views = h.ms.list_kind(scope, "memos", "memo").map_err(lib_error)?;
    let storage = h.ms.storage().map_err(lib_error)?;
    views
        .iter()
        .map(|view| {
            let raw = storage.read_message(&view.message_id).map_err(lib_error)?;
            Ok(memo_json(view, &raw))
        })
        .collect()
}

pub(super) fn show(h: &Hosted, arguments: &Value) -> Result<Value, ToolError> {
    let handle = req_str(arguments, "handle")?;
    let storage = h.ms.storage().map_err(lib_error)?;
    let Located {
        view,
        raw,
        events,
        kind,
    } = locate(&storage, handle)?;
    if kind == "memo" {
        return Ok(memo_json(&view, &raw));
    }
    let deps = if is_work(&kind) {
        Some(h.ms.task_depends_on(&view.message_id).map_err(lib_error)?)
    } else {
        None
    };
    Ok(json!({
        "item": item_row(&kind, &view, &events, deps),
        "text": render_message(&raw).unwrap_or_default(),
        "letter": to_json_message(&view.message_id, &raw).ok(),
    }))
}

pub(super) fn thread(h: &Hosted, arguments: &Value) -> Result<Value, ToolError> {
    let handle = req_str(arguments, "handle")?;
    let limit = opt_usize(arguments, "limit", THREAD_DEFAULT, THREAD_MAX)?;
    let messages = h.ms.thread(handle, false, limit, 50).map_err(lib_error)?;
    let seed = messages
        .iter()
        .find(|m| m.handle.eq_ignore_ascii_case(handle) || m.message_id == handle)
        .or_else(|| messages.first());
    let state = seed.map(|m| status_for_role(&m.role));
    let event =
        h.ms.item_event_state(handle)
            .map_err(lib_error)?
            .map(|s| json!({ "state": s.state, "event": s.event, "heads": s.heads }));
    let rows: Vec<Value> = messages
        .iter()
        .map(|m| {
            let kind = m.kind.as_deref().unwrap_or("mail");
            let mut item = json!({
                "handle": m.handle,
                "message_id": m.message_id,
                "kind": kind,
                "from": m.from,
                "to": m.to,
                "cc": m.cc,
                "subject": m.subject,
                "role": m.role,
                "date": m.date,
                "marks": m.marks,
                "body": m.body.trim(),
            });
            if is_work(kind) {
                item["status"] = json!(status_for_role(&m.role));
            }
            if let Some(branch) = &m.close_branch {
                item["close_branch"] = json!(branch);
            }
            if let Some(commit) = &m.close_commit {
                item["close_commit"] = json!(commit);
            }
            item
        })
        .collect();
    Ok(json!({ "handle": handle, "state": state, "event": event, "messages": rows }))
}

/// The backlog graph, `None` until the mailspace has minted any work.
fn backlog(h: &Hosted) -> Result<Option<vivi::mailspace::GraphShow>, ToolError> {
    match h.ms.graph_show("backlog") {
        Ok(show) => Ok(Some(show)),
        Err(error) if error.to_string().contains("not found") => Ok(None),
        Err(error) => Err(lib_error(error)),
    }
}

fn node_json(node: &vivi::mailspace::GraphNodeView) -> Value {
    json!({
        "handle": node.source_id,
        "subject": node.label,
        "kind": node.kind,
        "state": node.state,
        "readiness": node.readiness,
        "blocked_by": node.blocked_by,
    })
}

pub(super) fn graph(h: &Hosted, arguments: &Value) -> Result<Value, ToolError> {
    let view = opt_str(arguments, "view")?.unwrap_or("status");
    if !matches!(view, "ready" | "status") {
        return Err(ToolError::new("view must be ready or status"));
    }
    let Some(show) = backlog(h)? else {
        return Ok(
            json!({ "ready": [], "blocked": [], "done": [], "totals": { "ready": 0, "blocked": 0, "done": 0 } }),
        );
    };
    let ready: Vec<Value> = show
        .ready
        .iter()
        .filter(|n| !is_gate_kind(&n.kind))
        .map(node_json)
        .collect();
    let gates: Vec<Value> = show
        .ready
        .iter()
        .filter(|n| is_gate_kind(&n.kind))
        .map(node_json)
        .collect();
    let blocked: Vec<Value> = show.blocked.iter().map(node_json).collect();
    if view == "ready" {
        return Ok(json!({ "ready": ready, "blocked": blocked, "gates": gates }));
    }
    let done: Vec<Value> = show
        .nodes
        .iter()
        .filter(|n| n.state == "done")
        .map(node_json)
        .collect();
    Ok(json!({
        "ready": ready,
        "blocked": blocked,
        "gates": gates,
        "done": done,
        "totals": { "ready": ready.len(), "blocked": blocked.len(), "done": done.len() },
    }))
}

/// `vivi board --json`, as the mailspace builds it, bounded: a faberlang-scale
/// project holds thousands of open items whose last events carry whole
/// receipts. Tasks, needs and wants are capped per role (newest first, the
/// rest counted as hidden), each last-event note is cut to a preview, and the
/// graph view lists a capped frontier without its node table. `full` lifts
/// every bound; an explicit cap still wins. The host's filesystem root is
/// dropped from the answer.
pub(super) fn board(h: &Hosted, arguments: &Value) -> Result<Value, ToolError> {
    let flag = |name: &str| {
        arguments
            .get(name)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let full = flag("full");
    let cap_default = |bounded: usize| if full { usize::MAX } else { bounded };
    let wants_cap = opt_usize(
        arguments,
        "wants_cap",
        cap_default(BOARD_WANTS_DEFAULT),
        usize::MAX,
    )?;
    let caps = BoardCaps {
        tasks: opt_usize(
            arguments,
            "tasks_cap",
            cap_default(BOARD_ITEMS_DEFAULT),
            usize::MAX,
        )?,
        needs: opt_usize(
            arguments,
            "needs_cap",
            cap_default(BOARD_ITEMS_DEFAULT),
            usize::MAX,
        )?,
        graph: opt_usize(
            arguments,
            "graph_cap",
            cap_default(BOARD_GRAPH_DEFAULT),
            usize::MAX,
        )?,
        notes: full || flag("notes"),
        nodes: full,
    };
    let for_identity = opt_str(arguments, "for")?.map(str::to_owned);
    let board =
        h.ms.board(&BoardOptions {
            for_identity,
            wants_cap,
            since: None,
            with_process: false,
            with_graph: flag("graph"),
            with_pump: flag("pump"),
        })
        .map_err(lib_error)?;
    let mut value = serde_json::to_value(&board).map_err(|e| ToolError::new(e.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        object.remove("root");
    }
    bound_board(&mut value, &caps);
    Ok(value)
}

/// How far `board` trims what the library returns.
struct BoardCaps {
    tasks: usize,
    needs: usize,
    graph: usize,
    /// Keep each last event's whole `note`.
    notes: bool,
    /// Keep each graph's full node table.
    nodes: bool,
}

/// Trim a serialized board in place: see [`board`].
fn bound_board(board: &mut Value, caps: &BoardCaps) {
    let mut totals_hidden = (0, 0);
    if let Some(identities) = board.get_mut("identities").and_then(Value::as_array_mut) {
        for identity in identities {
            let tasks = cap_items(identity, "tasks", caps.tasks, caps.notes);
            let needs = cap_items(identity, "needs", caps.needs, caps.notes);
            if let Some(object) = identity.as_object_mut() {
                object.insert("tasks_hidden".into(), json!(tasks));
                object.insert("needs_hidden".into(), json!(needs));
            }
            if let Some(wants) = identity.get_mut("wants").and_then(Value::as_array_mut) {
                wants
                    .iter_mut()
                    .for_each(|item| trim_note(item, caps.notes));
            }
            totals_hidden = (totals_hidden.0 + tasks, totals_hidden.1 + needs);
        }
    }
    if let Some(totals) = board.get_mut("totals").and_then(Value::as_object_mut) {
        totals.insert("tasks_hidden".into(), json!(totals_hidden.0));
        totals.insert("needs_hidden".into(), json!(totals_hidden.1));
    }
    if let Some(graphs) = board.get_mut("graphs").and_then(Value::as_array_mut) {
        for graph in graphs.iter_mut().filter_map(Value::as_object_mut) {
            for view in ["ready", "blocked", "active"] {
                if let Some(Value::Array(nodes)) = graph.get_mut(view) {
                    let total = nodes.len();
                    nodes.truncate(caps.graph);
                    graph.insert(format!("{view}_total"), json!(total));
                }
            }
            if !caps.nodes
                && let Some(Value::Array(nodes)) = graph.remove("nodes")
            {
                graph.insert("nodes_total".into(), json!(nodes.len()));
            }
        }
    }
}

/// Sort one role's list newest first, keep the first `cap`, trim the notes of
/// what stays, and return how many were cut.
fn cap_items(identity: &mut Value, list: &str, cap: usize, notes: bool) -> usize {
    let Some(items) = identity.get_mut(list).and_then(Value::as_array_mut) else {
        return 0;
    };
    items.sort_by(|a, b| item_date(b).cmp(item_date(a)));
    let hidden = items.len().saturating_sub(cap);
    items.truncate(cap);
    items.iter_mut().for_each(|item| trim_note(item, notes));
    hidden
}

fn item_date(item: &Value) -> &str {
    item.get("date").and_then(Value::as_str).unwrap_or("")
}

/// Replace an item's `last_event.note` with `note_preview` and `note_chars`,
/// unless the caller asked for whole notes.
fn trim_note(item: &mut Value, keep: bool) {
    if keep {
        return;
    }
    let Some(event) = item.get_mut("last_event").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(Value::String(note)) = event.remove("note") else {
        return;
    };
    let chars = note.chars().count();
    let mut preview: String = note.chars().take(NOTE_PREVIEW_CHARS).collect();
    if chars > NOTE_PREVIEW_CHARS {
        preview.push('\u{2026}');
    }
    event.insert("note_preview".into(), json!(preview));
    event.insert("note_chars".into(), json!(chars));
}

pub(super) fn overview(h: &Hosted, project_id: &str) -> Result<Value, ToolError> {
    let board =
        h.ms.board(&BoardOptions {
            for_identity: Some(h.actor.clone()),
            wants_cap: usize::MAX,
            ..BoardOptions::default()
        })
        .map_err(lib_error)?;
    let mut recent = Vec::new();
    // Every open item this caller holds: the totals count these, and so does
    // `ready`.
    let mut held: HashSet<&str> = HashSet::new();
    if let Some(mine) = board.identities.first() {
        held.extend(
            [&mine.needs, &mine.tasks, &mine.wants]
                .into_iter()
                .flatten()
                .map(|item| item.handle.as_str()),
        );
        for (kind, items) in [
            ("need", &mine.needs),
            ("task", &mine.tasks),
            ("want", &mine.wants),
        ] {
            let shown = if kind == "want" { RECENT } else { usize::MAX };
            for item in items.iter().take(shown) {
                recent.push(json!({
                    "handle": item.handle,
                    "kind": kind,
                    "from": item.from,
                    "subject": item.subject,
                    "date": item.date,
                    "status": "open",
                    "last_event": item.last_event.as_ref().map(|e| e.command.clone()),
                }));
            }
        }
    }
    let skip = recent.len().saturating_sub(RECENT);
    let recent: Vec<Value> = recent.into_iter().skip(skip).collect();
    // The backlog frontier spans every role's open work; the totals above
    // are this caller's own. Only the frontier items the caller holds are its
    // ready work (`graph` shows the project's whole frontier).
    let mut ready: Vec<String> = match backlog(h)? {
        Some(show) => frontier_from_show(&show).ready,
        None => Vec::new(),
    };
    ready.retain(|handle| held.contains(handle.as_str()));
    let ready_total = ready.len();
    ready.truncate(OVERVIEW_READY_CAP);
    let mail_visible = h.ms.list(&h.actor, "inbox").map_err(lib_error)?.len();
    let memos =
        h.ms.list_kind(Some(&h.actor), "memos", "memo")
            .map_err(lib_error)?
            .len();
    let address = h.ms.address_for(&h.actor);
    let prefix = address
        .rsplit_once('@')
        .and_then(|(_, domain)| prefix_of_domain(domain))
        .map(str::to_owned);
    Ok(json!({
        "you": {
            "name": h.actor,
            "mailspace_id": h.ms.mailspace_id(),
            "prefix": prefix,
            "address": address,
        },
        "project_id": project_id,
        "totals": {
            "needs_open": board.totals.needs_open,
            "tasks_open": board.totals.tasks_open,
            "wants_open": board.totals.wants_open,
            "mail_visible": mail_visible,
            "memos": memos,
            "ready": ready_total,
        },
        "ready": ready,
        "ready_total": ready_total,
        "recent": recent,
    }))
}
