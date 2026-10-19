use serde::Serialize;
use vivi::VivariumError;
use vivi::mailspace::{Mailspace, is_foreign_account};
use vivi::storage::{MailspaceEvent, StoredMessageView};

#[derive(Debug, Serialize)]
struct WorkListItem {
    handle: String,
    kind: String,
    status: String,
    role: String,
    /// The address a held item is held under; own roles carry none.
    #[serde(skip_serializing_if = "Option::is_none")]
    held_for: Option<String>,
    date: String,
    from: String,
    to: String,
    subject: String,
    marks: Vec<String>,
    last_event: Option<WorkListEvent>,
}

#[derive(Debug, Serialize)]
struct WorkListEvent {
    occurred_at: String,
    command: String,
    event_type: String,
    actor_identity: Option<String>,
    note: Option<String>,
}

/// Which held work a listing shows: rows this mailspace holds under another
/// mailspace's address rather than under one of its own roles.
#[derive(Debug)]
pub(crate) enum HeldScope {
    /// Exactly one foreign address.
    For(String),
    /// Every other mailspace's address.
    All,
}

impl HeldScope {
    fn holds(&self, account: &str) -> bool {
        match self {
            HeldScope::For(address) => account.eq_ignore_ascii_case(address),
            HeldScope::All => is_foreign_account(account),
        }
    }
}

/// Resolve the list flags into a held scope, or `None` for this mailspace's
/// own folders. `--for <address>` (any value with an `@`) and `--held` both
/// ask for held work; asking both ways at once is refused.
pub(crate) fn held_scope(
    for_identity: Option<&str>,
    held: bool,
) -> Result<Option<HeldScope>, VivariumError> {
    if held {
        if for_identity.is_some() {
            return Err(VivariumError::Message(
                "--held and --for both scope the list; use one of them".into(),
            ));
        }
        return Ok(Some(HeldScope::All));
    }
    Ok(for_identity
        .filter(|value| value.contains('@'))
        .map(|value| HeldScope::For(value.trim().to_ascii_lowercase())))
}

pub(crate) fn print_work_lists(
    mailspace: &Mailspace,
    identity: Option<&str>,
    roles: &[&str],
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    marks: &[String],
    json: bool,
    pump: bool,
) -> Result<(), VivariumError> {
    let from = crate::local_mail_list::resolve_list_header(mailspace, from);
    let to = crate::local_mail_list::resolve_list_header(mailspace, to);
    let mut messages = Vec::new();
    for role in roles {
        // The sender's own copies of work handed to another mailspace are
        // the author's open outbound work; only those, not all sent mail.
        let listed = if *role == "sent" {
            mailspace.list_outbound_work(identity, kind)?
        } else {
            let mut listed = mailspace.list_kind(identity, role, kind)?;
            listed.extend(mailspace.assigned_work_for_role(identity, role, kind)?);
            listed
        };
        messages.extend(listed.into_iter().filter(|message| {
            (roles.contains(&"done") || message.absorbed_at.is_none())
                && crate::local_mail_list::headers_match(
                    &message.from_addr,
                    &message.to_addr,
                    &message.cc_addr,
                    from.as_ref(),
                    to.as_ref(),
                )
        }));
    }
    render_work_list(mailspace, messages, kind, roles, marks, json, pump)
}

/// List work this mailspace holds for another mailspace, across every folder
/// the status names. `sent` is this mailspace's own copy of handed-off work,
/// never a held row, so it is dropped here.
pub(crate) fn print_held_work_lists(
    mailspace: &Mailspace,
    scope: &HeldScope,
    roles: &[&str],
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    marks: &[String],
    json: bool,
    pump: bool,
) -> Result<(), VivariumError> {
    let from = crate::local_mail_list::resolve_list_header(mailspace, from);
    let to = crate::local_mail_list::resolve_list_header(mailspace, to);
    let folders: Vec<&str> = roles
        .iter()
        .copied()
        .filter(|role| *role != "sent")
        .collect();
    let mut messages = Vec::new();
    for folder in &folders {
        let listed = mailspace.list_kind(None, folder, kind)?;
        let storage = mailspace.storage()?;
        for message in listed {
            if scope.holds(&message.account)
                && !mailspace.is_locally_assigned_row(&storage, &message)?
                && (folders.contains(&"done") || message.absorbed_at.is_none())
                && crate::local_mail_list::headers_match(
                    &message.from_addr,
                    &message.to_addr,
                    &message.cc_addr,
                    from.as_ref(),
                    to.as_ref(),
                )
            {
                messages.push(message);
            }
        }
    }
    render_work_list(mailspace, messages, kind, &folders, marks, json, pump)
}

fn render_work_list(
    mailspace: &Mailspace,
    messages: Vec<StoredMessageView>,
    kind: &str,
    roles: &[&str],
    marks: &[String],
    json: bool,
    pump: bool,
) -> Result<(), VivariumError> {
    let storage = mailspace.storage()?;
    let message_ids = messages
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<Vec<_>>();
    let events_by_message = storage.list_mailspace_events_for_messages(&message_ids)?;
    let items = messages
        .into_iter()
        .map(|message| {
            let events = events_by_message
                .get(&message.message_id)
                .map_or([].as_slice(), Vec::as_slice);
            work_list_item(message, kind, events)
        })
        .filter(|item| vivi::mailspace::marks_include(&item.marks, marks))
        .collect::<Vec<_>>();
    if json {
        print_json(&items)
    } else {
        let suffixes = if pump {
            pump_suffixes(mailspace, &items)
        } else {
            Default::default()
        };
        print_human(kind, roles, &items, &suffixes);
        Ok(())
    }
}

/// `handle -> "pump 3/4 merged, open"` for the needs this host's pump has
/// planned. A missing or unreadable database yields no suffixes.
fn pump_suffixes(
    mailspace: &Mailspace,
    items: &[WorkListItem],
) -> std::collections::BTreeMap<String, String> {
    let Some(home) = mailspace
        .config
        .pump
        .as_ref()
        .map(|config| config.home_dir(&mailspace.root))
    else {
        return Default::default();
    };
    let vivi::pump::report::PumpRead::Ready(snapshot) =
        vivi::pump::report::read(&home).unwrap_or(vivi::pump::report::PumpRead::Missing)
    else {
        return Default::default();
    };
    items
        .iter()
        .filter_map(|item| {
            let summary = snapshot.need(&item.handle)?;
            let merged = summary.counts.get("merged").copied().unwrap_or(0);
            let state = if summary.amendment_pending {
                "amending"
            } else if summary.complete {
                "complete"
            } else {
                "open"
            };
            Some((
                item.handle.clone(),
                format!(
                    "pump r{} {merged}/{} merged, {state}",
                    summary.revision, summary.units_total
                ),
            ))
        })
        .collect()
}

fn work_list_item(
    message: StoredMessageView,
    kind: &str,
    events: &[MailspaceEvent],
) -> WorkListItem {
    let held_for = is_foreign_account(&message.account).then(|| message.account.clone());
    WorkListItem {
        handle: message.handle,
        kind: kind.into(),
        status: status_for_role(&message.local_role),
        role: message.local_role,
        held_for,
        date: message.date,
        from: message.from_addr,
        to: message.to_addr,
        subject: message.subject,
        marks: vivi::mailspace::folded_marks(events),
        last_event: events.last().map(work_list_event),
    }
}

fn work_list_event(event: &MailspaceEvent) -> WorkListEvent {
    WorkListEvent {
        occurred_at: event.occurred_at.clone(),
        command: event.command.clone(),
        event_type: event.event_type.clone(),
        actor_identity: event.actor_identity.clone(),
        note: event.note.clone(),
    }
}

fn status_for_role(role: &str) -> String {
    if role == "done" {
        "done".into()
    } else {
        "open".into()
    }
}

#[derive(Debug, Serialize)]
struct TaskListItem {
    handle: String,
    status: String,
    role: String,
    date: String,
    from: String,
    to: String,
    subject: String,
    depends_on: Vec<String>,
    last_event: Option<WorkListEvent>,
}

pub(crate) fn print_task_list(
    mailspace: &Mailspace,
    tasks: &[(StoredMessageView, Vec<String>)],
    json: bool,
) -> Result<(), VivariumError> {
    let storage = mailspace.storage()?;
    let message_ids = tasks
        .iter()
        .map(|(message, _)| message.message_id.clone())
        .collect::<Vec<_>>();
    let events_by_message = storage.list_mailspace_events_for_messages(&message_ids)?;
    let items = tasks
        .iter()
        .map(|(message, deps)| {
            let events = events_by_message
                .get(&message.message_id)
                .map_or([].as_slice(), Vec::as_slice);
            task_list_item(message.clone(), deps, events)
        })
        .collect::<Vec<_>>();
    if json {
        print_json(&items)
    } else {
        print_task_human(&items);
        Ok(())
    }
}

fn task_list_item(
    message: StoredMessageView,
    deps: &[String],
    events: &[MailspaceEvent],
) -> TaskListItem {
    TaskListItem {
        handle: message.handle,
        status: status_for_role(&message.local_role),
        role: message.local_role,
        date: message.date,
        from: message.from_addr,
        to: message.to_addr,
        subject: message.subject,
        depends_on: deps.to_vec(),
        last_event: events.last().map(work_list_event),
    }
}

fn print_task_human(items: &[TaskListItem]) {
    if items.is_empty() {
        println!("  no tasks");
        return;
    }
    println!("  handle  status  depends_on  subject  last_event");
    for item in items {
        let last_event = item
            .last_event
            .as_ref()
            .map_or("-", |event| event.command.as_str());
        println!(
            "  {}  {}  [{}]  {}  {}",
            item.handle,
            item.status,
            item.depends_on.join(", "),
            item.subject,
            last_event
        );
    }
}

fn print_json(items: &[impl serde::Serialize]) -> Result<(), VivariumError> {
    println!(
        "{}",
        serde_json::to_string_pretty(items)
            .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
    );
    Ok(())
}

fn print_human(
    kind: &str,
    roles: &[&str],
    items: &[WorkListItem],
    pump: &std::collections::BTreeMap<String, String>,
) {
    if items.is_empty() {
        let shown: Vec<&str> = roles
            .iter()
            .copied()
            .filter(|role| *role != "sent")
            .collect();
        println!("  no {kind}s in {}", shown.join("/"));
        return;
    }
    if items.iter().any(|item| item.held_for.is_some()) {
        print_held_human(items, pump);
        return;
    }
    println!("  handle  status  role  date  from  subject  last_event");
    for item in items {
        let last_event = item
            .last_event
            .as_ref()
            .map_or("-", |event| event.command.as_str());
        let suffix = pump
            .get(&item.handle)
            .map_or(String::new(), |text| format!("  [{text}]"));
        println!(
            "  {}  {}  {}  {}  {}  {}  {}{suffix}",
            item.handle, item.status, item.role, item.date, item.from, item.subject, last_event
        );
    }
}

/// The same rows with the foreign address each is held under, after the
/// role column; listings of own roles print exactly as before.
fn print_held_human(items: &[WorkListItem], pump: &std::collections::BTreeMap<String, String>) {
    println!("  handle  status  role  held_for  date  from  subject  last_event");
    for item in items {
        let last_event = item
            .last_event
            .as_ref()
            .map_or("-", |event| event.command.as_str());
        let suffix = pump
            .get(&item.handle)
            .map_or(String::new(), |text| format!("  [{text}]"));
        println!(
            "  {}  {}  {}  {}  {}  {}  {}  {}{suffix}",
            item.handle,
            item.status,
            item.role,
            item.held_for.as_deref().unwrap_or("-"),
            item.date,
            item.from,
            item.subject,
            last_event
        );
    }
}
