//! Board chat badge: one bounded unread probe across joined rooms.
//!
//! The probe never advances read cursors (unread means messages after the
//! stored cursor) and falls silent on any failure or budget exhaustion, so
//! board output is unchanged whenever chat is unconfigured, off, or the
//! server is unreachable.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::chat::b64;
use crate::chat::client::normalize_room_id;
use crate::chat::identity::ChatIdentity;
use crate::chat::state::{ChatState, JoinedRoom};
use crate::chat::{ChatConfig, chat_dir};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;

/// Total network budget across every room, so the board stays fast.
const BUDGET: Duration = Duration::from_millis(1500);
/// Character cap for the last-message snippet in the badge line.
const SNIPPET_CHARS: usize = 48;
/// Mirrors the state module's private database file name; its absence means
/// no room was ever joined, so the board never mints chat state.
const STATE_FILE: &str = "state.sqlite";

/// Probe every joined room for unread messages and return the badge line:
/// `chat: N unread in <room> (last: "<snippet>")`, or `chat: no new
/// messages` when every polled room is read. Returns `None` (silent) when
/// chat is unconfigured or off, no room is joined, the server is
/// unreachable, or the budget ran out before any unread was seen.
#[must_use]
pub fn unread_summary(project_root: &Path) -> Option<String> {
    let mailspace = match Mailspace::discover(Some(project_root)) {
        Ok(mailspace) => mailspace,
        Err(_) => return None,
    };
    let Some(config) = mailspace.config.chat.as_ref() else {
        return None;
    };
    probe(&mailspace, config).unwrap_or(None)
}

/// The bytes the board appends after its sections: the badge line plus a
/// newline when present, nothing at all otherwise, so chat-absent board
/// output stays byte-identical.
#[must_use]
pub fn badge_fragment(line: Option<&str>) -> String {
    line.filter(|line| !line.is_empty())
        .map(|line| format!("{line}\n"))
        .unwrap_or_default()
}

/// Local gates before any network: notify flag, configured server, existing
/// state, at least one joined room.
fn probe(mailspace: &Mailspace, config: &ChatConfig) -> Result<Option<String>, VivariumError> {
    if !config.notify {
        return Ok(None);
    }
    let Some(server) = normalized_server(config) else {
        return Ok(None);
    };
    let dir = chat_dir(&mailspace.dir);
    if !dir.join(STATE_FILE).exists() {
        return Ok(None);
    }
    let state = ChatState::open(&dir)?;
    let rooms = state.rooms()?;
    if rooms.is_empty() {
        return Ok(None);
    }
    let identity = ChatIdentity::ensure(&dir)?;
    let probes = room_probes(&state, &rooms);
    run_bounded(&server, &identity, &probes)
}

fn normalized_server(config: &ChatConfig) -> Option<String> {
    let server = config.server.as_deref()?.trim().trim_end_matches('/');
    (!server.is_empty()).then(|| server.to_string())
}

/// Per-room poll inputs: the room row plus the stored cursor to poll after.
fn room_probes<'a>(state: &ChatState, rooms: &'a [JoinedRoom]) -> Vec<(&'a JoinedRoom, u64)> {
    rooms
        .iter()
        .map(|room| (room, stored_cursor(state, &room.room_id)))
        .collect()
}

/// The stored read cursor as a poll offset; unread counts from there.
fn stored_cursor(state: &ChatState, room_id: &str) -> u64 {
    match state.cursor(room_id) {
        Ok(Some(raw)) => raw.parse().unwrap_or(0),
        _ => 0,
    }
}

/// Run the network probe on one thread with a current-thread runtime (the
/// judgment-provider precedent), sharing one HTTP client across all rooms.
fn run_bounded(
    server: &str,
    identity: &ChatIdentity,
    probes: &[(&JoinedRoom, u64)],
) -> Result<Option<String>, VivariumError> {
    let server = server.to_string();
    let identity = identity.clone();
    let probes = probes
        .iter()
        .map(|(room, since)| ((*room).clone(), *since))
        .collect::<Vec<_>>();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| VivariumError::Other(format!("failed to build runtime: {e}")))?;
        runtime.block_on(probe_all(&server, &identity, &probes))
    });
    let summary = handle
        .join()
        .map_err(|_| VivariumError::Message("chat probe thread panicked".into()))??;
    Ok(summary)
}

/// Poll rooms until the budget is spent; the first room with unread wins the
/// badge line. A failure or exhausted budget stops early: an unread already
/// seen still reports, otherwise the badge stays silent rather than guess.
async fn probe_all(
    server: &str,
    identity: &ChatIdentity,
    probes: &[(JoinedRoom, u64)],
) -> Result<Option<String>, VivariumError> {
    let http = reqwest::Client::builder()
        .build()
        .map_err(|e| VivariumError::Other(format!("failed to build client: {e}")))?;
    let deadline = Instant::now() + BUDGET;
    let mut unread: Option<(String, usize, String)> = None;
    let mut completed = true;
    for (room, since) in probes {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            completed = false;
            break;
        }
        match poll_room(&http, server, identity, room, *since, remaining).await {
            Ok((count, snippet)) => {
                if unread.is_none() && count > 0 {
                    unread = Some((room.room_id.clone(), count, snippet));
                }
            }
            Err(_) => {
                completed = false;
                break;
            }
        }
    }
    let line = match unread {
        Some((room, count, snippet)) => Some(format!(
            "chat: {count} unread in {room} (last: \"{snippet}\")"
        )),
        None if completed => Some("chat: no new messages".into()),
        None => None,
    };
    Ok(line)
}

/// Mint a per-room bearer and poll one page after `since` with `wait=0`,
/// without advancing the stored cursor. Each request's timeout is bounded
/// by the remaining probe budget.
async fn poll_room(
    http: &reqwest::Client,
    server: &str,
    identity: &ChatIdentity,
    room: &JoinedRoom,
    since: u64,
    budget: Duration,
) -> Result<(usize, String), ()> {
    let token = mint_bearer_bounded(http, server, identity, &room.room_id, budget).await?;
    let room_id = normalize_room_id(&room.room_id).map_err(|_| ())?;
    let url = format!("{server}/v1/rooms/{room_id}/messages?since={since}&wait=0");
    let response = http
        .get(&url)
        .timeout(budget)
        .header("Authorization", format!("Bearer {token}"))
        .header("X-Vivi-Chat-From", &room.label)
        .header("X-Vivi-Chat-Fingerprint", identity.short_wire_fingerprint())
        .send()
        .await
        .map_err(|_| ())?;
    if response.status().as_u16() != 200 {
        return Err(());
    }
    let page: Value = response.json().await.map_err(|_| ())?;
    let messages = page
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(())?
        .clone();
    let snippet = messages
        .last()
        .map(|message| snippet_of(message))
        .unwrap_or_default();
    Ok((messages.len(), snippet))
}

/// Challenge → sign → token bearer mint against the shared client, with the
/// remaining budget as each request's timeout.
async fn mint_bearer_bounded(
    http: &reqwest::Client,
    server: &str,
    identity: &ChatIdentity,
    room_id: &str,
    budget: Duration,
) -> Result<String, ()> {
    let room_id = normalize_room_id(room_id).map_err(|_| ())?;
    let challenge_url = format!("{server}/v1/rooms/{room_id}/challenge");
    let body = post_json_bounded(
        http,
        &challenge_url,
        &json!({ "fingerprint": identity.wire_fingerprint() }),
        budget,
    )
    .await?;
    let challenge_id = body.get("challenge_id").and_then(Value::as_str).ok_or(())?;
    let challenge = body.get("challenge").and_then(Value::as_str).ok_or(())?;
    let bytes = b64::decode(challenge).ok_or(())?;
    if bytes.len() != 32 {
        return Err(());
    }
    let token_url = format!("{server}/v1/rooms/{room_id}/auth/token");
    let body = post_json_bounded(
        http,
        &token_url,
        &json!({
            "challenge_id": challenge_id,
            "signature_base64": b64::encode(&identity.sign(&bytes)),
        }),
        budget,
    )
    .await?;
    body.get("token")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or(())
}

async fn post_json_bounded(
    http: &reqwest::Client,
    url: &str,
    payload: &Value,
    timeout: Duration,
) -> Result<Value, ()> {
    let response = http
        .post(url)
        .timeout(timeout)
        .json(payload)
        .send()
        .await
        .map_err(|_| ())?;
    if response.status().as_u16() != 200 {
        return Err(());
    }
    response.json().await.map_err(|_| ())
}

/// Collapse a message to one short line fragment (body, else subject).
fn snippet_of(message: &Value) -> String {
    let body = message
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source = if body.is_empty() {
        message
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else {
        body
    };
    snippet(source)
}

/// Flatten whitespace and cap the length with an ellipsis.
fn snippet(body: &str) -> String {
    let flattened = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= SNIPPET_CHARS {
        return flattened;
    }
    let mut cut: String = flattened.chars().take(SNIPPET_CHARS).collect();
    cut.push_str("...");
    cut
}

#[cfg(test)]
#[path = "board_probe_test.rs"]
mod tests;
