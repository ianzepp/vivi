//! Chat CLI handlers for the whole `vivi chat` command family, dispatched
//! from the binary. Every command flushes the outbox opportunistically
//! first.

use std::path::Path;

use crate::chat::identity::ChatIdentity;
use crate::chat::state::ChatState;
use crate::chat::{chat_dir, client, outbox, sync, watch};
use crate::cli::{ChatSubcommand, RoomSubcommand};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;

/// Dispatch one `vivi chat` subcommand.
pub fn dispatch(command: &ChatSubcommand) -> Result<(), VivariumError> {
    match command {
        ChatSubcommand::Status { project } => status(project.as_deref()),
        ChatSubcommand::Join {
            room_id,
            label,
            project,
        } => join(room_id, label.as_deref(), project.as_deref()),
        ChatSubcommand::Rooms { project } => rooms(project.as_deref()),
        ChatSubcommand::Send {
            room_id,
            to,
            subject,
            body,
            body_file,
            project,
        } => send(
            room_id,
            to.as_deref(),
            subject.as_deref(),
            body.as_deref(),
            body_file.as_deref(),
            project.as_deref(),
        ),
        ChatSubcommand::Flush { project } => flush(project.as_deref()),
        ChatSubcommand::Read {
            room_id,
            mark_read,
            all,
            project,
        } => read(room_id.as_deref(), *mark_read, *all, project.as_deref()),
        ChatSubcommand::Watch {
            room_id,
            once,
            project,
        } => watch_command(room_id.as_deref(), *once, project.as_deref()),
        ChatSubcommand::Pull { room_id, project } => {
            pull_command(room_id.as_deref(), project.as_deref())
        }
        ChatSubcommand::Leave { room_id, project } => leave(room_id, project.as_deref()),
        ChatSubcommand::Room { command } => match command {
            RoomSubcommand::Create {
                seats,
                label,
                project,
            } => room_create(*seats, label.clone(), project.as_deref()),
        },
    }
}

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

/// `vivi chat join`: challenge → sign → seat binding, recorded locally.
pub fn join(
    room_id: &str,
    label: Option<&str>,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    try_flush(&mailspace);
    let server = server_of(&mailspace)?;
    let label = label
        .map(str::to_string)
        .or_else(|| {
            mailspace
                .config
                .chat
                .as_ref()
                .and_then(|chat| chat.name.clone())
        })
        .unwrap_or_else(|| client::default_label(&mailspace.root));
    let chat_dir = chat_dir(&mailspace.dir);
    let identity = ChatIdentity::ensure(&chat_dir)?;
    let receipt = client::join_room(&server, room_id, &identity, &label)?;
    let state = ChatState::open(&chat_dir)?;
    state.join_room(&receipt.room_id, receipt.seat, &receipt.label)?;
    println!(
        "joined {} seat {} as {}",
        receipt.room_id, receipt.seat, receipt.label
    );
    println!("fingerprint {}", receipt.fingerprint);
    println!("token expires {}", receipt.token_expires_at);
    Ok(())
}

/// `vivi chat rooms`: list locally joined rooms.
pub fn rooms(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    try_flush(&mailspace);
    let server = mailspace
        .config
        .chat
        .as_ref()
        .and_then(|chat| chat.server.clone())
        .unwrap_or_else(|| crate::chat::DEFAULT_SERVER.to_string());
    let state = ChatState::open(&chat_dir(&mailspace.dir))?;
    for room in state.rooms()? {
        println!(
            "{}  seat {}  {}  {}",
            room.room_id, room.seat, room.label, server
        );
    }
    Ok(())
}

/// `vivi chat status`: identity, server, joined rooms, outbox depth.
pub fn status(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    try_flush(&mailspace);
    let chat_dir = chat_dir(&mailspace.dir);
    let identity = ChatIdentity::ensure(&chat_dir)?;
    let state = ChatState::open(&chat_dir)?;
    let server = mailspace
        .config
        .chat
        .as_ref()
        .and_then(|chat| chat.server.clone())
        .unwrap_or_else(|| crate::chat::DEFAULT_SERVER.to_string());
    println!("chat identity: {}", identity.fingerprint());
    println!("chat server: {server}");
    println!("chat rooms joined: {}", state.joined_room_count()?);
    for room in state.rooms()? {
        println!(
            "  room {}  seat {}  {}",
            room.room_id, room.seat, room.label
        );
    }
    println!("chat outbox depth: {}", state.outbox_depth()?);
    Ok(())
}

/// `vivi chat watch`: long-poll arrivals in one or every joined room.
pub fn watch_command(
    room_id: Option<&str>,
    once: bool,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let server = server_of(&mailspace)?;
    let (state, identity) = chat_parts(&mailspace)?;
    try_flush(&mailspace);
    let rooms = watch::resolve_rooms(&state, room_id)?;
    watch::run(&server, &state, &identity, &rooms, once)
}

/// `vivi chat pull`: file new room items into the mailspace. One room's
/// failure is reported and the others still run; the first error is returned.
pub fn pull_command(room_id: Option<&str>, project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let server = server_of(&mailspace)?;
    sync::resolve_recipient(&mailspace)?;
    let (state, identity) = chat_parts(&mailspace)?;
    try_flush(&mailspace);
    let mut failure = None;
    for room in watch::resolve_rooms(&state, room_id)? {
        match sync::pull(&server, &mailspace, &state, &identity, &room, false) {
            Ok(report) => println!("{}", report.summary()),
            Err(error) => {
                eprintln!("pull {}: {error}", room.room_id);
                failure.get_or_insert(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

/// `vivi chat leave`: local unjoin (drops the room row and its cursor; the
/// server seat stays bound).
pub fn leave(room_id: &str, project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    try_flush(&mailspace);
    let (state, _identity) = chat_parts(&mailspace)?;
    let room_id = client::normalize_room_id(room_id)?;
    if state.leave_room(&room_id)? {
        println!("left {room_id}");
    } else {
        println!("room {room_id} was not joined");
    }
    Ok(())
}

/// `vivi chat room create`: mint a room on the server; the creator key
/// auto-binds seat 1, which is stored locally for immediate send/watch.
pub fn room_create(
    seats: Option<u32>,
    label: Option<String>,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let server = server_of(&mailspace)?;
    try_flush(&mailspace);
    let (state, identity) = chat_parts(&mailspace)?;
    let label = label
        .or_else(|| {
            mailspace
                .config
                .chat
                .as_ref()
                .and_then(|chat| chat.name.clone())
        })
        .unwrap_or_else(|| client::default_label(&mailspace.root));
    let receipt = client::create_room(&server, seats, Some(&label), &identity)?;
    state.join_room(&receipt.room_id, receipt.seat, &receipt.label)?;
    println!("created {}", receipt.room_id);
    println!("seat {} label {}", receipt.seat, receipt.label);
    println!("fingerprint {}", receipt.fingerprint);
    println!("share the room id above to invite seats");
    Ok(())
}
