//! `vivi chat watch`: long-poll each joined room and print arrivals live.
//! Ctrl-C ends the process cleanly — cursors advance after every cycle, so
//! nothing replays on the next invocation.

use std::time::Duration;

use crate::chat::client;
use crate::chat::identity::ChatIdentity;
use crate::chat::outbox;
use crate::chat::state::{ChatState, JoinedRoom};
use crate::error::VivariumError;

/// Server long-poll hold per cycle (contract `wait` query param, seconds).
pub const POLL_WAIT_SECS: u64 = 25;

/// Backoff after a failed cycle in loop mode, so an unreachable server does
/// not spin the loop.
const ERROR_BACKOFF: Duration = Duration::from_secs(1);

/// One poll-and-print cycle for a room: mint a fresh bearer, long-poll since
/// the stored cursor, advance the cursor. Returns the rendered lines so the
/// caller prints exactly what arrived (and tests can see them).
///
/// # Errors
/// Returns an error when the bearer mint, the poll, or the state store fails.
pub fn cycle(
    server: &str,
    state: &ChatState,
    identity: &ChatIdentity,
    room: &JoinedRoom,
    wait_secs: u64,
) -> Result<Vec<String>, VivariumError> {
    let since = state
        .cursor(&room.room_id)?
        .and_then(|cursor| cursor.parse::<u64>().ok())
        .unwrap_or(0);
    let token = client::mint_bearer(server, &room.room_id, identity)?;
    let page = client::poll_messages(
        server,
        &room.room_id,
        &token,
        &room.label,
        &identity.short_wire_fingerprint(),
        since,
        wait_secs,
    )?;
    state.set_cursor(&room.room_id, &page.next_cursor.to_string())?;
    Ok(page.messages.iter().map(outbox::render_message).collect())
}

/// Watch loop over `rooms`: poll each room per cycle and print arrivals as
/// they land. `once` exits after the first cycle (fail-fast loops); loop
/// mode reports per-room errors and keeps watching.
///
/// # Errors
/// In `once` mode a cycle error propagates; loop mode only errors never.
pub fn run(
    server: &str,
    state: &ChatState,
    identity: &ChatIdentity,
    rooms: &[JoinedRoom],
    once: bool,
) -> Result<(), VivariumError> {
    println!("watching {} room(s); Ctrl-C to exit", rooms.len());
    loop {
        for room in rooms {
            match cycle(server, state, identity, room, POLL_WAIT_SECS) {
                Ok(lines) => {
                    for line in lines {
                        println!("{line}");
                    }
                }
                Err(error) if !once => {
                    eprintln!("watch: {error}");
                    std::thread::sleep(ERROR_BACKOFF);
                }
                Err(error) => return Err(error),
            }
        }
        if once {
            return Ok(());
        }
    }
}

/// Resolve which rooms a watch invocation covers: the one named room, or
/// every joined room.
///
/// # Errors
/// Returns an error when the named room is not joined or no room is joined.
pub fn resolve_rooms(
    state: &ChatState,
    room_id: Option<&str>,
) -> Result<Vec<JoinedRoom>, VivariumError> {
    match room_id {
        Some(raw) => {
            let room_id = client::normalize_room_id(raw)?;
            let room = state.room(&room_id)?.ok_or_else(|| {
                VivariumError::Message(format!(
                    "room '{room_id}' is not joined; run vivi chat join first"
                ))
            })?;
            Ok(vec![room])
        }
        None => {
            let rooms = state.rooms()?;
            if rooms.is_empty() {
                return Err(VivariumError::Message(
                    "no rooms joined; run vivi chat join first".into(),
                ));
            }
            Ok(rooms)
        }
    }
}

#[cfg(test)]
#[path = "watch_test.rs"]
mod tests;
