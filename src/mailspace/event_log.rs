use super::SendRequest;
use super::delivery::DeliveredMessageId;
use crate::error::VivariumError;
use crate::storage::{MailspaceEventInput, Storage, StoredMessage, StoredMessageView};

pub(super) fn log_send_events(
    storage: &Storage,
    from: &str,
    request: &SendRequest,
    delivered: &[DeliveredMessageId],
    sent: &StoredMessage,
    parent: Option<&StoredMessageView>,
) -> Result<(), VivariumError> {
    let command = send_command(&request.role);
    append_event(
        storage,
        &EventDetails {
            command,
            event_type: "sent_copy_created",
            actor: Some(from),
            account: from,
            message_id: &sent.message_id,
            content_id: &sent.content_id,
            from_role: None,
            to_role: Some("sent"),
            from_identity: Some(from),
            to_identity: Some(from),
            subject: &request.subject,
            note: None,
        },
    )?;
    for delivered in delivered {
        append_event(
            storage,
            &EventDetails {
                command,
                event_type: "delivered",
                actor: Some(from),
                account: &delivered.identity,
                message_id: &delivered.message_id,
                content_id: &delivered.content_id,
                from_role: None,
                to_role: Some(&request.role),
                from_identity: Some(from),
                to_identity: Some(&delivered.identity),
                subject: &request.subject,
                note: None,
            },
        )?;
    }
    if let Some(parent) = parent {
        log_reply_on_parent(storage, from, request, parent)?;
    }
    Ok(())
}

/// Attach a `replied` event to every work-item copy of the parent so board
/// `last_event` moves when a thread reply lands.
fn log_reply_on_parent(
    storage: &Storage,
    from: &str,
    request: &SendRequest,
    parent: &StoredMessageView,
) -> Result<(), VivariumError> {
    let command = reply_command(&request.role);
    let mut ids = storage.message_ids_by_content(&parent.content_id)?;
    if !ids.iter().any(|id| id == &parent.message_id) {
        ids.push(parent.message_id.clone());
    }
    ids.sort();
    ids.dedup();
    for message_id in ids {
        append_event(
            storage,
            &EventDetails {
                command,
                event_type: "replied",
                actor: Some(from),
                account: &parent.account,
                message_id: &message_id,
                content_id: &parent.content_id,
                from_role: None,
                to_role: Some(&parent.local_role),
                from_identity: Some(from),
                to_identity: Some(&parent.account),
                subject: &parent.subject,
                note: Some(&request.subject),
            },
        )?;
    }
    Ok(())
}

pub(super) fn log_move_event(
    storage: &Storage,
    identity: &str,
    role: &str,
    before: &StoredMessageView,
    command: &str,
    note: Option<&str>,
) -> Result<(), VivariumError> {
    append_event(
        storage,
        &EventDetails {
            command,
            event_type: "moved",
            actor: Some(identity),
            account: identity,
            message_id: &before.message_id,
            content_id: &before.content_id,
            from_role: Some(&before.local_role),
            to_role: Some(role),
            from_identity: Some(identity),
            to_identity: Some(identity),
            subject: &before.subject,
            note,
        },
    )
}

fn send_command(role: &str) -> &'static str {
    match role {
        "tasks" => "task send",
        "needs" => "need send",
        "wants" => "want send",
        _ => "mail send",
    }
}

fn reply_command(role: &str) -> &'static str {
    match role {
        "tasks" => "task send",
        "needs" => "need send",
        "wants" => "want send",
        _ => "mail reply",
    }
}

struct EventDetails<'a> {
    command: &'a str,
    event_type: &'a str,
    actor: Option<&'a str>,
    account: &'a str,
    message_id: &'a str,
    content_id: &'a str,
    from_role: Option<&'a str>,
    to_role: Option<&'a str>,
    from_identity: Option<&'a str>,
    to_identity: Option<&'a str>,
    subject: &'a str,
    note: Option<&'a str>,
}

fn append_event(storage: &Storage, details: &EventDetails<'_>) -> Result<(), VivariumError> {
    storage.append_mailspace_event(&MailspaceEventInput {
        command: details.command.into(),
        event_type: details.event_type.into(),
        actor_identity: details.actor.map(str::to_string),
        account: details.account.into(),
        message_id: details.message_id.into(),
        content_id: details.content_id.into(),
        from_role: details.from_role.map(str::to_string),
        to_role: details.to_role.map(str::to_string),
        from_identity: details.from_identity.map(str::to_string),
        to_identity: details.to_identity.map(str::to_string),
        subject: details.subject.into(),
        note: details.note.map(str::to_string),
    })?;
    Ok(())
}
