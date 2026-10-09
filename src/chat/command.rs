//! Chat CLI handlers for send/flush/read. Dispatched from the binary's
//! chat command family; every command flushes the outbox opportunistically
//! first.

use std::path::Path;

use crate::chat::identity::ChatIdentity;
use crate::chat::state::ChatState;
use crate::chat::{chat_dir, client, outbox};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;

fn server_of(mailspace: &Mailspace) -> Result<String, VivariumError> {
    mailspace
        .config
        .chat
        .as_ref()
        .and_then(|chat| chat.server.clone())
        .filter(|server| !server.trim().is_empty())
        .ok_or_else(|| {
            VivariumError::Config(
                "no chat server configured; set [chat] server in mailspace.toml".into(),
            )
        })
}

fn chat_parts(mailspace: &Mailspace) -> Result<(ChatState, ChatIdentity), VivariumError> {
    let dir = chat_dir(&mailspace.dir);
    let identity = ChatIdentity::ensure(&dir)?;
    let state = ChatState::open(&dir)?;
    Ok((state, identity))
}

/// `vivi chat send`: compose, enqueue transactionally, then attempt an
/// immediate flush. Prints `sent` or `queued` honestly; a queued message
/// retries on the next chat command or `vivi chat flush`.
pub fn send(
    room_id: &str,
    to: Option<&str>,
    subject: Option<&str>,
    body: Option<&str>,
    body_file: Option<&Path>,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let server = server_of(&mailspace)?;
    let (state, identity) = chat_parts(&mailspace)?;
    try_flush(&mailspace);
    let room_id = client::normalize_room_id(room_id)?;
    let body = match (body, body_file) {
        (Some(text), _) => text.to_string(),
        (None, Some(path)) => std::fs::read_to_string(path)?,
        (None, None) => {
            return Err(VivariumError::Message(
                "pass a message body with --body or --body-file".into(),
            ));
        }
    };
    let report = outbox::send_message(&server, &state, &identity, &room_id, to, subject, &body)?;
    if report.delivered {
        println!("sent {} to {}", report.message_id, room_id);
    } else {
        println!(
            "queued {} for {} ({}); retries on the next chat command or vivi chat flush",
            report.message_id,
            room_id,
            report
                .queued_reason
                .as_deref()
                .unwrap_or("not acknowledged")
        );
    }
    Ok(())
}

/// `vivi chat flush`: deliver every pending outbox row.
pub fn flush(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let server = server_of(&mailspace)?;
    let (state, identity) = chat_parts(&mailspace)?;
    let summary = outbox::deliver_all(&server, &state, &identity)?;
    println!(
        "flush: {} sent ({} duplicate), {} queued",
        summary.sent, summary.duplicates, summary.queued
    );
    if let Some(error) = summary.error {
        println!("first failure: {error}");
    }
    Ok(())
}

/// `vivi chat read`: poll since the stored cursor, render origin labels and
/// fingerprints, and advance the cursor with `--mark-read`.
pub fn read(
    room_id: Option<&str>,
    mark_read: bool,
    all: bool,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let server = server_of(&mailspace)?;
    let (state, identity) = chat_parts(&mailspace)?;
    try_flush(&mailspace);
    let room_id = match room_id {
        Some(raw) => client::normalize_room_id(raw)?,
        None => pick_default_room(&state)?,
    };
    let page = outbox::read_room(&server, &state, &identity, &room_id, mark_read, all)?;
    for message in &page.messages {
        println!("{}", outbox::render_message(message));
    }
    println!(
        "room {}: {} message(s), cursor {}{}",
        page.room_id,
        page.messages.len(),
        page.next_cursor,
        if page.advanced { "" } else { " (not advanced)" }
    );
    Ok(())
}

fn pick_default_room(state: &ChatState) -> Result<String, VivariumError> {
    let rooms = state.rooms()?;
    match rooms.len() {
        0 => Err(VivariumError::Message(
            "no rooms joined; run vivi chat join first".into(),
        )),
        1 => Ok(rooms[0].room_id.clone()),
        _ => Err(VivariumError::Message(format!(
            "{} rooms joined; pass a room id: {}",
            rooms.len(),
            rooms
                .iter()
                .map(|room| room.room_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Opportunistic outbox flush run before every chat command. Silent unless
/// it delivers something; failures keep messages queued.
pub fn try_flush(mailspace: &Mailspace) {
    let Ok(server) = server_of(mailspace) else {
        return;
    };
    let Ok((state, identity)) = chat_parts(mailspace) else {
        return;
    };
    if state.outbox_depth().unwrap_or(0) == 0 {
        return;
    }
    if let Ok(summary) = outbox::deliver_all(&server, &state, &identity) {
        if summary.sent > 0 {
            println!("flushed {} queued message(s)", summary.sent);
        }
    }
}
