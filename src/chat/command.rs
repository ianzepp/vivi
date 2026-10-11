//! Chat CLI handlers for the whole `vivi chat` command family, dispatched
//! from the binary. Every command flushes the outbox opportunistically
//! first.

use std::path::Path;

use crate::chat::identity::ChatIdentity;
use crate::chat::outbox::Draft;
use crate::chat::state::{ChatState, RejectedMessage};
use crate::chat::{chat_dir, client, outbox, prune, sync, watch};
use crate::cli::{ChatSendCommand, ChatSubcommand, RoomSubcommand};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::{is_traveling_id, short_handle};

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
        ChatSubcommand::Send(args) => send(args),
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
            pull_command(room_id.as_deref(), project.as_deref(), false)
        }
        ChatSubcommand::Resync { room_id, project } => {
            pull_command(room_id.as_deref(), project.as_deref(), true)
        }
        ChatSubcommand::Prune {
            room_id,
            older_than,
            dry_run,
            project,
        } => prune_command(room_id.as_deref(), older_than, *dry_run, project.as_deref()),
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
pub fn send(args: &ChatSendCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(args.project.as_deref())?;
    let server = server_of(&mailspace)?;
    let (state, identity) = chat_parts(&mailspace)?;
    try_flush(&mailspace);
    let room_id = client::normalize_room_id(&args.room_id)?;
    let body = match (args.body.as_deref(), args.body_file.as_deref()) {
        (Some(text), _) => text.to_string(),
        (None, Some(path)) => std::fs::read_to_string(path)?,
        (None, None) => {
            return Err(VivariumError::Message(
                "pass a message body with --body or --body-file".into(),
            ));
        }
    };
    let reply_to = args
        .reply_to
        .as_deref()
        .map(|token| resolve_citation(&mailspace, token))
        .transpose()?;
    let refs = args
        .refs
        .iter()
        .map(|token| resolve_citation(&mailspace, token))
        .collect::<Result<Vec<_>, _>>()?;
    let draft = Draft {
        to: args.to.as_deref(),
        subject: args.subject.as_deref(),
        body: &body,
        kind: args.kind.as_deref(),
        in_reply_to: reply_to.as_deref(),
        refs: &refs,
    };
    let report = outbox::send_message(&server, &state, &identity, &room_id, &draft)?;
    if let Some(reason) = &report.rejected {
        return Err(VivariumError::Message(format!(
            "the server rejected {} for {room_id}: {reason}; it stays in the outbox marked \
             rejected and is not retried (vivi chat status lists it)",
            report.message_id
        )));
    }
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
    println!("handle {}", short_handle(&report.message_id));
    Ok(())
}

/// Turn a handle or id the operator typed into the full id a send cites. A
/// full traveling id passes through even when this mailspace has not seen it
/// (a reference may name an item that lives elsewhere); anything else must
/// resolve here.
fn resolve_citation(mailspace: &Mailspace, token: &str) -> Result<String, VivariumError> {
    if is_traveling_id(token) {
        return Ok(token.to_string());
    }
    let id = mailspace.storage()?.resolve_message_token(token)?;
    if !is_traveling_id(&id) {
        return Err(VivariumError::Message(format!(
            "'{token}' has no citable id in this mailspace"
        )));
    }
    Ok(id)
}

/// `vivi chat flush`: deliver every pending outbox row.
pub fn flush(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let server = server_of(&mailspace)?;
    let (state, identity) = chat_parts(&mailspace)?;
    let summary = outbox::deliver_all(&server, &state, &identity)?;
    let rejected = state.rejected_outbox()?;
    println!(
        "flush: {} sent ({} duplicate), {} queued, {} rejected",
        summary.sent,
        summary.duplicates,
        summary.queued,
        rejected.len()
    );
    if let Some(error) = summary.error {
        println!("first failure: {error}");
    }
    print_rejected(&rejected);
    if rejected.is_empty() {
        Ok(())
    } else {
        Err(VivariumError::Message(format!(
            "{} message(s) rejected by the server stay in the outbox and are not retried",
            rejected.len()
        )))
    }
}

/// One line per message the server refused for good.
fn print_rejected(rejected: &[RejectedMessage]) {
    for item in rejected {
        println!(
            "rejected {} (room {}): {}",
            item.message_id, item.room_id, item.reason
        );
    }
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
    let rejected = state.rejected_outbox()?;
    if !rejected.is_empty() {
        println!("chat outbox rejected: {}", rejected.len());
        print_rejected(&rejected);
    }
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

/// `vivi chat pull` (and `resync`, which replays from cursor zero): file room
/// items into the mailspace. One room's failure is reported and the others
/// still run; the first error is returned.
///
/// # Errors
/// Returns an error when no server or `[chat] recipient` is configured, a
/// named room is not joined, or any room's pull fails.
pub fn pull_command(
    room_id: Option<&str>,
    project: Option<&Path>,
    from_zero: bool,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let server = server_of(&mailspace)?;
    sync::resolve_recipient(&mailspace)?;
    let (state, identity) = chat_parts(&mailspace)?;
    try_flush(&mailspace);
    let mut failure = None;
    for room in watch::resolve_rooms(&state, room_id)? {
        match sync::pull(&server, &mailspace, &state, &identity, &room, from_zero) {
            Ok(report) => {
                println!("{}", report.summary());
                warn_conflicts(&report);
            }
            Err(error) => {
                eprintln!("pull {}: {error}", room.room_id);
                failure.get_or_insert(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

/// `vivi chat prune`: evict old room items from the local cache. Purely
/// local: it needs neither the server nor a configured recipient.
///
/// # Errors
/// Returns an error for an unusable age or room id, or a storage failure.
pub fn prune_command(
    room_id: Option<&str>,
    older_than: &str,
    dry_run: bool,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let age = prune::parse_age(older_than)?;
    let report = prune::prune(&mailspace, room_id, age, chrono::Utc::now(), dry_run)?;
    let verb = if dry_run { "would evict" } else { "evicted" };
    println!(
        "{verb} {} item(s) dated before {} (open needs/tasks/wants, starred, absorbed, and local mail are kept)",
        if dry_run {
            report.items.len()
        } else {
            report.evicted
        },
        report.cutoff
    );
    for item in report.items.iter().take(PRUNE_LISTING) {
        println!(
            "  {} {} {} {}",
            item.handle, item.role, item.date, item.subject
        );
    }
    if report.items.len() > PRUNE_LISTING {
        println!("  ... and {} more", report.items.len() - PRUNE_LISTING);
    }
    if dry_run {
        println!("dry run: nothing changed");
    } else if report.evicted > 0 {
        println!("vivi chat resync restores evicted items from the room");
    }
    Ok(())
}

/// Lines of the eviction listing shown before summarizing the rest.
const PRUNE_LISTING: usize = 20;

/// Name the ids a pull refused, so a divergent or hostile item is seen.
fn warn_conflicts(report: &sync::PullReport) {
    if report.conflicts.is_empty() {
        return;
    }
    let shown = report.conflicts.iter().take(5).cloned().collect::<Vec<_>>();
    eprintln!(
        "warning: room {} sent {} item(s) reusing an id this mailspace already holds with \
         different content; refused: {}",
        report.room_id,
        report.conflicts.len(),
        shown.join(", ")
    );
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
