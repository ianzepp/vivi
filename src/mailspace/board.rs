//! The mailspace board: open work per identity, goals, and optional graph,
//! process and pump views. The CLI prints this model; a server serializes it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{GoalView, Mailspace};
use crate::error::VivariumError;
use crate::role_schedule::{ScheduleReport, ScheduleState};
use crate::role_status::ProcessReport;
use crate::storage::{MailspaceEvent, Storage, StoredMessageView};

mod wants;

/// What a board read includes and filters.
#[derive(Debug, Clone, Default)]
pub struct BoardOptions {
    /// Restrict the board to one identity (name or alias).
    pub for_identity: Option<String>,
    /// Cap on wants shown per identity; the rest are counted as hidden.
    pub wants_cap: usize,
    /// Keep only items whose date or latest event is at or after this time.
    pub since: Option<DateTime<Utc>>,
    /// Probe each role's live process.
    pub with_process: bool,
    /// Include the work-graph frontier.
    pub with_graph: bool,
}

#[derive(Debug, Serialize)]
pub struct Board {
    pub name: String,
    pub root: PathBuf,
    pub totals: BoardTotals,
    pub identities: Vec<IdentityBoard>,
    /// Registered goal document paths (always present).
    pub goals: Vec<GoalView>,
    /// Present when `board --graph` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graphs: Option<Vec<BoardGraph>>,
}

#[derive(Debug, Serialize)]
pub struct BoardGraph {
    pub handle: String,
    pub code: String,
    pub status: String,
    pub revision: i64,
    pub ready: Vec<BoardGraphNode>,
    pub blocked: Vec<BoardGraphNode>,
    pub active: Vec<BoardGraphNode>,
    pub nodes: Vec<BoardGraphNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoardGraphNode {
    pub handle: String,
    pub source_id: String,
    pub code: String,
    pub label: String,
    pub state: String,
    pub readiness: String,
    pub blocked_by: Vec<String>,
    pub successors: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct BoardTotals {
    pub actionable_open: usize,
    pub tasks_open: usize,
    pub needs_open: usize,
    pub wants_open: usize,
    pub wants_shown: usize,
    pub wants_hidden: usize,
}

#[derive(Debug, Serialize)]
pub struct IdentityBoard {
    pub identity: String,
    pub address: String,
    /// Configured model slug for the next role spawn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Configured reasoning effort for the next role spawn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    pub actionable_open: usize,
    pub tasks: Vec<BoardItem>,
    pub needs: Vec<BoardItem>,
    pub wants: Vec<BoardItem>,
    pub wants_hidden: usize,
    /// Live process status. Present only when the board is run with `--process`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessReport>,
    /// Schedule health from role cadence and latest outbound signal.
    pub schedule: ScheduleReport,
    /// Latest pump-dispatched run for this role, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_auto_run: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BoardItem {
    pub handle: String,
    pub date: String,
    pub from: String,
    pub subject: String,
    pub last_event: Option<BoardEvent>,
    /// The needs serving this want, with their state. Empty for tasks and
    /// needs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub serving: Vec<BoardServing>,
}

/// One need serving a want on the board; `state` is `open` or `done`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardServing {
    pub handle: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct BoardEvent {
    pub occurred_at: String,
    pub command: String,
    pub event_type: String,
    pub note: Option<String>,
}

impl Mailspace {
    /// Build the board: open tasks, needs and wants per identity, registered
    /// goals, and the optional graph, process and pump views.
    ///
    /// # Errors
    /// Returns an error when the identity does not resolve or storage cannot
    /// be read.
    pub fn board(&self, options: &BoardOptions) -> Result<Board, VivariumError> {
        build_board(self, options)
    }
}

fn build_board(mailspace: &Mailspace, options: &BoardOptions) -> Result<Board, VivariumError> {
    let BoardOptions {
        for_identity,
        wants_cap,
        since,
        with_process,
        with_graph,
    } = options.clone();
    let for_identity = for_identity.as_deref();
    let identities = board_identities(mailspace, for_identity)?;
    let pump_snapshot = read_pump_snapshot(mailspace);
    let cadence_runs = pump_snapshot
        .as_ref()
        .map(crate::pump::report::Snapshot::cadence_runs)
        .unwrap_or_default();
    let boards = build_identity_boards(
        mailspace,
        identities,
        &cadence_runs,
        wants_cap,
        since,
        with_process,
    )?;
    let graphs = if with_graph {
        Some(board_graphs(mailspace)?)
    } else {
        None
    };
    Ok(Board {
        name: mailspace.config.name.clone(),
        root: mailspace.root.clone(),
        totals: board_totals(&boards),
        identities: boards,
        goals: mailspace.goal_list()?,
        graphs,
    })
}

fn build_identity_boards(
    mailspace: &Mailspace,
    identities: Vec<String>,
    cadence_runs: &BTreeMap<String, crate::pump::report::CadenceRunSummary>,
    wants_cap: usize,
    since: Option<DateTime<Utc>>,
    with_process: bool,
) -> Result<Vec<IdentityBoard>, VivariumError> {
    let storage = mailspace.storage()?;
    let scopes = board_identity_scopes(mailspace, &identities);
    let account_identity = account_identity_map(&scopes);
    let accounts = unique_scope_accounts(&scopes);
    let mut messages = storage.list_messages_by_account_roles_raw(&accounts, &board_roles())?;
    messages.extend(wants::reopened_rows(&storage, &account_identity)?);
    apply_scoped_handles(&storage, &scopes, &account_identity, &mut messages)?;
    let ids: Vec<String> = messages
        .iter()
        .map(|message| message.message_id.clone())
        .collect();
    let events = storage.list_mailspace_events_for_messages(&ids)?;
    let mut grouped = group_board_messages(messages, &account_identity);
    identities
        .into_iter()
        .map(|identity| {
            build_identity_board(
                mailspace,
                &storage,
                &identity,
                grouped.remove(&identity).unwrap_or_default(),
                &events,
                cadence_runs,
                wants_cap,
                since,
                with_process,
            )
        })
        .collect()
}

/// The pump block: counts from `pump.db`, plus complete needs still open in
/// the board. Missing or unreadable state leaves the block absent.
fn read_pump_snapshot(mailspace: &Mailspace) -> Option<crate::pump::report::Snapshot> {
    let home = mailspace
        .config
        .pump
        .as_ref()
        .map(|config| config.home_dir(&mailspace.root))?;
    match crate::pump::report::read(&home) {
        Ok(crate::pump::report::PumpRead::Ready(snapshot)) => Some(snapshot),
        _ => None,
    }
}

fn board_graphs(mailspace: &Mailspace) -> Result<Vec<BoardGraph>, VivariumError> {
    let shows = mailspace.graph_board_summaries()?;
    Ok(shows
        .into_iter()
        .map(|show| {
            let nodes: Vec<BoardGraphNode> = show
                .nodes
                .iter()
                .map(|n| board_graph_node(n, &show))
                .collect();
            let ready = nodes
                .iter()
                .filter(|n| n.readiness == "ready")
                .cloned()
                .collect();
            let blocked = nodes
                .iter()
                .filter(|n| n.readiness == "blocked")
                .cloned()
                .collect();
            let active = nodes
                .iter()
                .filter(|n| n.state == "active")
                .cloned()
                .collect();
            BoardGraph {
                handle: show.graph.handle,
                code: show.graph.code,
                status: show.graph.status,
                revision: show.graph.current_revision,
                ready,
                blocked,
                active,
                nodes,
            }
        })
        .collect())
}

fn board_graph_node(
    node: &crate::mailspace::GraphNodeView,
    show: &crate::mailspace::GraphShow,
) -> BoardGraphNode {
    // Map source-id blocked_by/successors to handles for agent contracts.
    let by_source: HashMap<&str, &str> = show
        .nodes
        .iter()
        .map(|n| (n.source_id.as_str(), n.handle.as_str()))
        .collect();
    let blocked_by = node
        .blocked_by
        .iter()
        .map(|source| {
            by_source
                .get(source.as_str())
                .map_or_else(|| source.clone(), |h| (*h).to_string())
        })
        .collect();
    let successors = node
        .successors
        .iter()
        .map(|source| {
            by_source
                .get(source.as_str())
                .map_or_else(|| source.clone(), |h| (*h).to_string())
        })
        .collect();
    BoardGraphNode {
        handle: node.handle.clone(),
        source_id: node.source_id.clone(),
        code: node.source_id.clone(),
        label: node.label.clone(),
        state: node.state.clone(),
        readiness: node.readiness.clone(),
        blocked_by,
        successors,
    }
}

fn board_identities(
    mailspace: &Mailspace,
    for_identity: Option<&str>,
) -> Result<Vec<String>, VivariumError> {
    if let Some(identity) = for_identity {
        return Ok(vec![mailspace.resolve_identity(identity)?]);
    }
    Ok(mailspace
        .config
        .identities
        .iter()
        .map(|identity| identity.name.clone())
        .collect())
}

fn board_identity_scopes(
    mailspace: &Mailspace,
    identities: &[String],
) -> Vec<(String, Vec<String>)> {
    identities
        .iter()
        .map(|identity| {
            let mut names = mailspace
                .identity_names(identity)
                .into_iter()
                .collect::<Vec<_>>();
            names.sort();
            (identity.clone(), names)
        })
        .collect()
}

fn account_identity_map(scopes: &[(String, Vec<String>)]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (identity, accounts) in scopes {
        for account in accounts {
            map.insert(account.clone(), identity.clone());
        }
    }
    map
}

fn unique_scope_accounts(scopes: &[(String, Vec<String>)]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut accounts = Vec::new();
    for (_, scope_accounts) in scopes {
        for account in scope_accounts {
            if seen.insert(account.clone()) {
                accounts.push(account.clone());
            }
        }
    }
    accounts
}

fn board_roles() -> Vec<String> {
    ["tasks", "needs", "wants"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn apply_scoped_handles(
    storage: &Storage,
    scopes: &[(String, Vec<String>)],
    account_identity: &HashMap<String, String>,
    messages: &mut [StoredMessageView],
) -> Result<(), VivariumError> {
    let handles_by_identity = storage.display_handles_for_account_scopes(scopes)?;
    for message in messages {
        let Some(identity) = account_identity.get(&message.account) else {
            continue;
        };
        let Some(handles) = handles_by_identity.get(identity) else {
            continue;
        };
        if let Some(handle) = handles.get(&message.message_id) {
            message.handle = handle.clone();
        }
    }
    Ok(())
}

fn group_board_messages(
    messages: Vec<StoredMessageView>,
    account_identity: &HashMap<String, String>,
) -> HashMap<String, Vec<StoredMessageView>> {
    let mut grouped: HashMap<String, Vec<StoredMessageView>> = HashMap::new();
    for message in messages {
        if let Some(identity) = account_identity.get(&message.account) {
            grouped.entry(identity.clone()).or_default().push(message);
        }
    }
    grouped
}

#[allow(clippy::too_many_arguments)]
fn build_identity_board(
    mailspace: &Mailspace,
    storage: &Storage,
    identity: &str,
    messages: Vec<StoredMessageView>,
    events_by_message: &HashMap<String, Vec<MailspaceEvent>>,
    cadence_runs: &BTreeMap<String, crate::pump::report::CadenceRunSummary>,
    wants_cap: usize,
    since: Option<DateTime<Utc>>,
    with_process: bool,
) -> Result<IdentityBoard, VivariumError> {
    let (tasks_open, needs_open, wants_open) = partition_board_messages(messages);
    let (wants_open, mut serving) = wants::open_wants(storage, wants_open)?;
    let tasks = board_items(tasks_open, events_by_message, None, since);
    let needs = board_items(needs_open, events_by_message, None, since);
    let (mut wants, wants_count) =
        board_items_with_count(wants_open, events_by_message, Some(wants_cap), since);
    for want in &mut wants {
        want.serving = serving.remove(&want.handle).unwrap_or_default();
    }
    let (model, thinking) = role_capacity(mailspace, identity);
    let schedule = mailspace
        .schedule_report_with(storage, identity)
        .unwrap_or_else(|_| crate::role_schedule::evaluate(None, None, Utc::now()));
    let last_auto_run = if matches!(schedule.state, ScheduleState::None) {
        None
    } else {
        cadence_runs.get(identity).map(cadence_run_label)
    };
    Ok(IdentityBoard {
        identity: identity.into(),
        address: mailspace.address_for(identity),
        model,
        thinking,
        actionable_open: tasks.len() + needs.len(),
        wants_hidden: wants_count.saturating_sub(wants.len()),
        process: role_process_status(mailspace, identity, with_process),
        schedule,
        last_auto_run,
        tasks,
        needs,
        wants,
    })
}

fn role_capacity(mailspace: &Mailspace, identity: &str) -> (Option<String>, Option<String>) {
    mailspace
        .config
        .identities
        .iter()
        .find(|role| role.name == identity)
        .map_or((None, None), |role| {
            (role.model.clone(), role.thinking.clone())
        })
}

/// Look up a role's pid/host binding and probe it when `with_process` is set.
/// Reads the config record directly so the board does not pay charter file reads.
fn role_process_status(
    mailspace: &Mailspace,
    identity: &str,
    with_process: bool,
) -> Option<ProcessReport> {
    if !with_process {
        return None;
    }
    let role = mailspace
        .config
        .identities
        .iter()
        .find(|known| known.name == identity);
    let pid = role.and_then(|role| role.pid);
    let host = role.and_then(|role| role.host.as_deref());
    let harness = role.and_then(|role| role.harness.as_deref());
    Some(crate::role_status::probe_quick(pid, host, harness))
}

fn partition_board_messages(
    messages: Vec<StoredMessageView>,
) -> (
    Vec<StoredMessageView>,
    Vec<StoredMessageView>,
    Vec<StoredMessageView>,
) {
    let mut tasks = Vec::new();
    let mut needs = Vec::new();
    let mut wants = Vec::new();
    for message in messages {
        if message.absorbed_at.is_some() {
            continue;
        }
        match message.local_role.as_str() {
            "tasks" => tasks.push(message),
            "needs" => needs.push(message),
            "wants" => wants.push(message),
            _ => {}
        }
    }
    (tasks, needs, wants)
}

fn board_items(
    messages: Vec<StoredMessageView>,
    events_by_message: &HashMap<String, Vec<MailspaceEvent>>,
    cap: Option<usize>,
    since: Option<DateTime<Utc>>,
) -> Vec<BoardItem> {
    board_items_with_count(messages, events_by_message, cap, since).0
}

fn board_items_with_count(
    messages: Vec<StoredMessageView>,
    events_by_message: &HashMap<String, Vec<MailspaceEvent>>,
    cap: Option<usize>,
    since: Option<DateTime<Utc>>,
) -> (Vec<BoardItem>, usize) {
    let limit = cap.unwrap_or(usize::MAX);
    let mut items = Vec::new();
    let mut matching = 0;
    for message in messages {
        let events = events_by_message
            .get(&message.message_id)
            .map_or([].as_slice(), Vec::as_slice);
        if !changed_since(&message, events, since) {
            continue;
        }
        matching += 1;
        if items.len() < limit {
            items.push(board_item(message, events));
        }
    }
    (items, matching)
}

fn changed_since(
    message: &StoredMessageView,
    events: &[MailspaceEvent],
    since: Option<DateTime<Utc>>,
) -> bool {
    let Some(since) = since else {
        return true;
    };
    timestamp_at_or_after(&message.date, since)
        || events
            .iter()
            .any(|event| timestamp_at_or_after(&event.occurred_at, since))
}

fn timestamp_at_or_after(raw: &str, since: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(raw).is_ok_and(|date| date.with_timezone(&Utc) >= since)
}

fn board_item(message: StoredMessageView, events: &[MailspaceEvent]) -> BoardItem {
    BoardItem {
        handle: message.handle,
        date: message.date,
        from: message.from_addr,
        subject: message.subject,
        last_event: events.last().map(board_event),
        serving: Vec::new(),
    }
}

fn board_event(event: &MailspaceEvent) -> BoardEvent {
    BoardEvent {
        occurred_at: event.occurred_at.clone(),
        command: event.command.clone(),
        event_type: event.event_type.clone(),
        note: event.note.clone(),
    }
}

fn board_totals(identities: &[IdentityBoard]) -> BoardTotals {
    let mut totals = BoardTotals::default();
    for identity in identities {
        totals.actionable_open += identity.actionable_open;
        totals.tasks_open += identity.tasks.len();
        totals.needs_open += identity.needs.len();
        totals.wants_open += identity.wants.len() + identity.wants_hidden;
        totals.wants_shown += identity.wants.len();
        totals.wants_hidden += identity.wants_hidden;
    }
    totals
}

fn cadence_run_label(run: &crate::pump::report::CadenceRunSummary) -> String {
    let state = run.outcome.as_deref().unwrap_or("in-flight");
    let at = run.ended_at.unwrap_or(run.started_at);
    let at = match i64::try_from(at) {
        Ok(seconds) => DateTime::from_timestamp(seconds, 0).map(|date| date.to_rfc3339()),
        Err(_) => None,
    }
    .unwrap_or_else(|| at.to_string());
    format!("{state} at {at}")
}

#[cfg(test)]
#[path = "board_test.rs"]
mod tests;
