use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{Mailspace, parse_time_bound};
use crate::error::VivariumError;
use crate::storage::{MailspaceEvent, Storage};

#[derive(Debug, Clone)]
pub struct MailspaceWatchRequest {
    pub for_identity: String,
    pub kinds: String,
    pub events: String,
    pub statuses: Option<String>,
    pub match_from: Option<String>,
    pub match_subject_prefix: Option<String>,
    pub handle: Option<String>,
    pub until_count: usize,
    pub timeout: Option<String>,
    pub once: bool,
    pub since: Option<String>,
    pub cursor_file: Option<PathBuf>,
    pub write_cursor: bool,
    pub poll_interval: String,
    pub json: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailspaceWatchEvent {
    pub event: String,
    pub status: String,
    pub kind: String,
    pub handle: String,
    #[serde(rename = "for")]
    pub for_identity: String,
    pub from: String,
    pub subject: String,
    pub at: String,
    pub event_id: i64,
}

struct WatchFilters {
    identity: BTreeSet<String>,
    kinds: BTreeSet<String>,
    events: BTreeSet<String>,
    statuses: Option<BTreeSet<String>>,
    match_from: Option<String>,
    subject_prefix: Option<String>,
    content_id: Option<String>,
}

/// Watch for mailspace events matching the given request filters.
///
/// # Errors
/// Returns an error if filter preparation, event scanning, or cursor I/O
/// fails. Also returns an error on timeout if no matching event arrives.
#[allow(clippy::needless_pass_by_value)]
pub fn run_watch(
    mailspace: &Mailspace,
    request: MailspaceWatchRequest,
) -> Result<(), VivariumError> {
    let poll_interval = parse_duration(&request.poll_interval)?;
    let timeout = request.timeout.as_deref().map(parse_duration).transpose()?;
    let filters = prepare_filters(mailspace, &request)?;
    let cursor_path = request.cursor_file.as_deref();
    let mut cursor = initial_cursor(mailspace, cursor_path, request.since.as_deref())?;
    let started = Instant::now();
    let mut matched = 0usize;

    let mut graph_cursor = 0i64;
    loop {
        let storage = mailspace.storage()?;
        let events = storage.list_mailspace_events_after(cursor)?;
        let result = scan_events(&storage, &events, &filters, &request, cursor, matched)?;
        cursor = result.cursor;
        matched = result.matched;
        if result.done {
            write_cursor(cursor_path, request.write_cursor, cursor)?;
            return Ok(());
        }
        if filters.kinds.contains("graph") {
            let graph_result =
                scan_graph_events(&storage, &filters, &request, graph_cursor, matched)?;
            graph_cursor = graph_result.cursor;
            matched = graph_result.matched;
            if graph_result.done {
                write_cursor(cursor_path, request.write_cursor, cursor)?;
                return Ok(());
            }
        }
        if request.once {
            if !events.is_empty() {
                write_cursor(cursor_path, request.write_cursor, cursor)?;
            }
            return Ok(());
        }
        if timeout.is_some_and(|duration| started.elapsed() >= duration) {
            return Err(VivariumError::Message(
                "mailspace watch timed out without a matching event".into(),
            ));
        }
        std::thread::sleep(poll_interval);
    }
}

struct ScanResult {
    cursor: i64,
    matched: usize,
    done: bool,
}

fn scan_events(
    storage: &Storage,
    events: &[MailspaceEvent],
    filters: &WatchFilters,
    request: &MailspaceWatchRequest,
    mut cursor: i64,
    mut matched: usize,
) -> Result<ScanResult, VivariumError> {
    for event in events {
        cursor = event.event_id;
        if !matches_event(storage, event, filters)? {
            continue;
        }
        let output = watch_event(storage, event)?;
        emit_event(&output, request.json)?;
        matched += 1;
        if request.until_count > 0 && matched >= request.until_count {
            return Ok(ScanResult {
                cursor,
                matched,
                done: true,
            });
        }
    }
    Ok(ScanResult {
        cursor,
        matched,
        done: false,
    })
}

fn emit_event(event: &MailspaceWatchEvent, json: bool) -> Result<(), VivariumError> {
    if json {
        println!(
            "{}",
            serde_json::to_string(event)
                .map_err(|e| VivariumError::Other(format!("failed to encode watch event: {e}")))?
        );
    } else {
        println!(
            "event={} status={} kind={} handle={} for={} from={} subject={} at={}",
            event.event,
            event.status,
            event.kind,
            event.handle,
            event.for_identity,
            event.from,
            event.subject,
            event.at
        );
    }
    Ok(())
}

fn prepare_filters(
    mailspace: &Mailspace,
    request: &MailspaceWatchRequest,
) -> Result<WatchFilters, VivariumError> {
    let storage = mailspace.storage()?;
    let identity = mailspace.resolve_identity(&request.for_identity)?;
    let identity = mailspace.identity_names(&identity).into_iter().collect();
    let content_id = request
        .handle
        .as_deref()
        .map(|handle| storage.resolve_message_token(handle))
        .transpose()?
        .map(|message_id| {
            storage.message_by_id(&message_id).and_then(|message| {
                message.map(|message| message.content_id).ok_or_else(|| {
                    VivariumError::Message(format!("message not found: {message_id}"))
                })
            })
        })
        .transpose()?;
    Ok(WatchFilters {
        identity,
        kinds: parse_filter_set(
            &request.kinds,
            "kind",
            &["mail", "task", "need", "want", "graph"],
        )?,
        events: parse_filter_set(
            &request.events,
            "event",
            &[
                "delivered",
                "moved",
                "sent_copy_created",
                "node_ready",
                "node_state",
                "attempt_bound",
                "revision_imported",
                "revision_applied",
            ],
        )?,
        statuses: request
            .statuses
            .as_deref()
            .map(|value| {
                parse_filter_set(
                    value,
                    "status",
                    &["tasks", "needs", "wants", "done", "inbox", "sent", "open"],
                )
            })
            .transpose()?,
        match_from: request.match_from.as_deref().map(str::to_ascii_lowercase),
        subject_prefix: request.match_subject_prefix.clone(),
        content_id,
    })
}

fn scan_graph_events(
    storage: &Storage,
    filters: &WatchFilters,
    request: &MailspaceWatchRequest,
    mut cursor: i64,
    mut matched: usize,
) -> Result<ScanResult, VivariumError> {
    let events = storage.list_work_graph_events_after(cursor)?;
    for event in events {
        cursor = event.event_id;
        if !filters.events.contains(&event.event_type) {
            continue;
        }
        let handle = event
            .node_handle
            .clone()
            .unwrap_or_else(|| event.graph_handle.clone());
        let output = MailspaceWatchEvent {
            event: event.event_type.clone(),
            status: "open".into(),
            kind: "graph".into(),
            handle,
            for_identity: request.for_identity.clone(),
            from: "graph".into(),
            subject: event.note.clone().unwrap_or_default(),
            at: event.occurred_at.clone(),
            event_id: event.event_id,
        };
        emit_event(&output, request.json)?;
        matched += 1;
        if request.until_count > 0 && matched >= request.until_count {
            return Ok(ScanResult {
                cursor,
                matched,
                done: true,
            });
        }
    }
    Ok(ScanResult {
        cursor,
        matched,
        done: false,
    })
}

fn matches_event(
    storage: &Storage,
    event: &MailspaceEvent,
    filters: &WatchFilters,
) -> Result<bool, VivariumError> {
    let event_for = event
        .to_identity
        .as_deref()
        .unwrap_or(event.account.as_str());
    let kind = event_kind(event);
    let status = event_status(event);
    Ok(filters.identity.contains(event_for)
        && filters.kinds.contains(&kind)
        && filters.events.contains(&event.event_type)
        && filters
            .statuses
            .as_ref()
            .is_none_or(|statuses| statuses.contains(&status))
        && filters.match_from.as_ref().is_none_or(|from| {
            event
                .from_identity
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase()
                == *from
        })
        && filters
            .subject_prefix
            .as_ref()
            .is_none_or(|prefix| event.subject.starts_with(prefix))
        && filters
            .content_id
            .as_ref()
            .is_none_or(|content_id| content_id == &event.content_id)
        && storage.message_by_id(&event.message_id)?.is_some())
}

fn watch_event(
    storage: &Storage,
    event: &MailspaceEvent,
) -> Result<MailspaceWatchEvent, VivariumError> {
    Ok(MailspaceWatchEvent {
        event: event.event_type.clone(),
        status: event_status(event),
        kind: event_kind(event),
        handle: storage.display_handle(&event.message_id)?,
        for_identity: event
            .to_identity
            .clone()
            .unwrap_or_else(|| event.account.clone()),
        from: event
            .from_identity
            .clone()
            .or_else(|| event.actor_identity.clone())
            .unwrap_or_default(),
        subject: event.subject.clone(),
        at: event.occurred_at.clone(),
        event_id: event.event_id,
    })
}

/// The item kind an event is about: the first word of the command that wrote
/// it (`need send`, `task done`). A letter filed by a sync pull records the
/// command `sync pull`, which names no kind, so its kind is the one its
/// delivery role files under (`needs` is a need, `inbox` is mail).
fn event_kind(event: &MailspaceEvent) -> String {
    let word = event.command.split_whitespace().next().unwrap_or("mail");
    if word != "sync" {
        return word.to_string();
    }
    match event.to_role.as_deref() {
        Some("needs") => "need",
        Some("tasks") => "task",
        Some("wants") => "want",
        _ => "mail",
    }
    .to_string()
}

fn event_status(event: &MailspaceEvent) -> String {
    event.to_role.clone().unwrap_or_else(|| "inbox".into())
}

fn initial_cursor(
    mailspace: &Mailspace,
    cursor_path: Option<&Path>,
    since: Option<&str>,
) -> Result<i64, VivariumError> {
    if let Some(path) = cursor_path
        && path.exists()
    {
        let raw = fs::read_to_string(path)?;
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(0);
        }
        return raw.parse::<i64>().map_err(|_| {
            VivariumError::Config(format!("malformed mailspace cursor: {}", path.display()))
        });
    }
    let Some(since) = since else { return Ok(0) };
    let bound = parse_time_bound(since)?.to_rfc3339();
    mailspace.storage()?.event_cursor_before(&bound)
}

fn write_cursor(path: Option<&Path>, enabled: bool, cursor: i64) -> Result<(), VivariumError> {
    if !enabled {
        return Ok(());
    }
    let Some(path) = path else {
        return Err(VivariumError::Config(
            "--write-cursor requires --cursor-file or --watermark-file".into(),
        ));
    };
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, format!("{cursor}\n"))?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn parse_filter_set(
    value: &str,
    label: &str,
    allowed: &[&str],
) -> Result<BTreeSet<String>, VivariumError> {
    let mut values = BTreeSet::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        if !allowed.contains(&item) {
            return Err(VivariumError::Config(format!(
                "unsupported {label} '{item}'; expected {}",
                allowed.join(", ")
            )));
        }
        values.insert(item.to_string());
    }
    if values.is_empty() {
        return Err(VivariumError::Config(format!(
            "{label} filter cannot be empty"
        )));
    }
    Ok(values)
}

pub fn parse_duration(value: &str) -> Result<Duration, VivariumError> {
    let value = value.trim();
    let (number, unit) = value
        .chars()
        .partition::<String, _>(|ch| ch.is_ascii_digit() || *ch == '.');
    let number = number
        .parse::<f64>()
        .map_err(|_| VivariumError::Config(format!("invalid duration '{value}'")))?;
    let seconds = match unit.as_str() {
        "ms" => number / 1_000.0,
        "s" | "" => number,
        "m" => number * 60.0,
        "h" => number * 3_600.0,
        "d" => number * 86_400.0,
        _ => return Err(VivariumError::Config(format!("invalid duration '{value}'"))),
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(VivariumError::Config(format!("invalid duration '{value}'")));
    }
    Ok(Duration::from_secs_f64(seconds))
}

#[cfg(test)]
#[path = "watch_test.rs"]
mod tests;
