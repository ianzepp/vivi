use serde::Serialize;
use vivi::VivariumError;
use vivi::mailspace::Mailspace;
use vivi::storage::{MailspaceEvent, StoredMessageView};

#[derive(Debug, Serialize)]
struct WorkListItem {
    handle: String,
    kind: String,
    status: String,
    role: String,
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

pub(crate) fn print_work_lists(
    mailspace: &Mailspace,
    identity: Option<&str>,
    roles: &[&str],
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    marks: &[String],
    json: bool,
) -> Result<(), VivariumError> {
    let storage = mailspace.storage()?;
    let from = crate::local_mail_list::resolve_list_header(mailspace, from);
    let to = crate::local_mail_list::resolve_list_header(mailspace, to);
    let mut messages = Vec::new();
    for role in roles {
        messages.extend(
            mailspace
                .list_kind(identity, role, kind)?
                .into_iter()
                .filter(|message| {
                    (roles.contains(&"done") || message.absorbed_at.is_none())
                        && crate::local_mail_list::headers_match(
                            &message.from_addr,
                            &message.to_addr,
                            &message.cc_addr,
                            from.as_ref(),
                            to.as_ref(),
                        )
                }),
        );
    }
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
        print_human(kind, roles, &items);
        Ok(())
    }
}

fn work_list_item(
    message: StoredMessageView,
    kind: &str,
    events: &[MailspaceEvent],
) -> WorkListItem {
    WorkListItem {
        handle: message.handle,
        kind: kind.into(),
        status: status_for_role(&message.local_role),
        role: message.local_role,
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

fn print_human(kind: &str, roles: &[&str], items: &[WorkListItem]) {
    if items.is_empty() {
        let role = roles.join("/");
        println!("  no {kind}s in {role}");
        return;
    }
    println!("  handle  status  role  date  from  subject  last_event");
    for item in items {
        let last_event = item
            .last_event
            .as_ref()
            .map_or("-", |event| event.command.as_str());
        println!(
            "  {}  {}  {}  {}  {}  {}  {}",
            item.handle, item.status, item.role, item.date, item.from, item.subject, last_event
        );
    }
}
