//! Outbox operations: compose with a traveling id, enqueue transactionally,
//! deliver at-least-once with fresh per-invocation bearers, and poll since
//! the stored cursor.

use std::collections::HashMap;

use rand_core::{OsRng, RngCore};
use serde_json::{Value, json};

use crate::chat::client::{self, ChatMessage};
use crate::chat::identity::ChatIdentity;
use crate::chat::item::work_kind;
use crate::chat::state::ChatState;
use crate::error::VivariumError;
use crate::storage::mint_traveling_id;

/// Server body cap (contract: sends over 64 KiB are rejected 413).
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Result of one send invocation: `delivered` tells sent from queued.
#[derive(Debug)]
pub struct SendReport {
    pub message_id: String,
    pub delivered: bool,
    pub queued_reason: Option<String>,
    pub summary: FlushSummary,
}

/// What one flush pass delivered. `sent_ids` carries acknowledged
/// Message-IDs so callers can attribute a specific send.
#[derive(Debug)]
pub struct FlushSummary {
    pub sent: usize,
    pub duplicates: usize,
    pub queued: usize,
    pub sent_ids: Vec<String>,
    pub error: Option<String>,
}

impl FlushSummary {
    fn empty() -> Self {
        Self {
            sent: 0,
            duplicates: 0,
            queued: 0,
            sent_ids: Vec::new(),
            error: None,
        }
    }
}

/// One read page for a room.
#[derive(Debug)]
pub struct ReadPage {
    pub room_id: String,
    pub messages: Vec<ChatMessage>,
    pub next_cursor: u64,
    pub advanced: bool,
}

/// Server cap on a cited Message-ID (contract R8: non-empty, <= 256 bytes).
const MAX_ID_BYTES: usize = 256;

/// Server cap on `refs` entries (contract R8).
const MAX_REFS: usize = 32;

/// One message to send: addressing, content, and the work-item kind and
/// threading that ride with it (contract R7/R8).
#[derive(Debug, Clone, Copy, Default)]
pub struct Draft<'a> {
    pub to: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub body: &'a str,
    /// `need`, `task`, or `want`; `None` is plain chat.
    pub kind: Option<&'a str>,
    /// Message-ID this item answers.
    pub in_reply_to: Option<&'a str>,
    /// Message-IDs this item cites.
    pub refs: &'a [String],
}

impl<'a> Draft<'a> {
    /// A plain chat message: no kind, no threading.
    #[must_use]
    pub fn chat(to: Option<&'a str>, subject: Option<&'a str>, body: &'a str) -> Self {
        Self {
            to,
            subject,
            body,
            ..Self::default()
        }
    }

    /// Check the draft against the server's rules so a bad send is refused
    /// here, before it is enqueued, instead of sitting in the outbox as a
    /// row the server will never accept.
    ///
    /// # Errors
    /// Returns an error naming the first rule the draft breaks.
    pub fn validate(&self) -> Result<(), VivariumError> {
        if let Some(kind) = self.kind
            && work_kind(Some(kind)).is_none()
        {
            return Err(VivariumError::Message(format!(
                "unsupported kind '{kind}'; expected need, task, or want"
            )));
        }
        if let Some(parent) = self.in_reply_to {
            check_id("reply target", parent)?;
        }
        if self.refs.len() > MAX_REFS {
            return Err(VivariumError::Message(format!(
                "{} refs is over the {MAX_REFS} ref limit",
                self.refs.len()
            )));
        }
        self.refs.iter().try_for_each(|id| check_id("ref", id))
    }
}

fn check_id(what: &str, id: &str) -> Result<(), VivariumError> {
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return Err(VivariumError::Message(format!(
            "a {what} must be 1 to {MAX_ID_BYTES} bytes, got {}",
            id.len()
        )));
    }
    Ok(())
}

/// Compose an outbox envelope with a fresh traveling id.
///
/// The id is minted here, once, and is both the wire `Message-ID` (the
/// server's idempotency key) and the mailspace message id other stores
/// import at ingest: one namespace, so a message keeps one short handle on
/// every machine. Retries re-send the stored id; nothing downstream re-mints.
/// `kind`, `in_reply_to`, and `refs` are stored only when set, so a plain
/// chat envelope is unchanged from phase 01.
#[must_use]
pub fn compose_message(draft: &Draft<'_>, identity: &ChatIdentity) -> (String, Value) {
    let mut entropy = [0u8; 16];
    OsRng.fill_bytes(&mut entropy);
    let message_id = mint_traveling_id(&identity.short_wire_fingerprint(), &entropy);
    let mut envelope = json!({
        "message_id": message_id,
        "to": draft.to,
        "subject": draft.subject,
        "body": draft.body,
    });
    if let Some(kind) = work_kind(draft.kind) {
        envelope["kind"] = json!(kind);
    }
    if let Some(parent) = draft.in_reply_to {
        envelope["in_reply_to"] = json!(parent);
    }
    if !draft.refs.is_empty() {
        envelope["refs"] = json!(draft.refs);
    }
    (message_id, envelope)
}

/// Enqueue one message for `room_id`, then attempt immediate delivery of
/// the whole outbox. Failure to deliver leaves the row queued (at-least-once).
///
/// # Errors
/// Returns an error when the room is not joined, the draft breaks a server
/// rule or the body cap, or the state store fails; never for a mere delivery
/// failure.
pub fn send_message(
    server: &str,
    state: &ChatState,
    identity: &ChatIdentity,
    room_id: &str,
    draft: &Draft<'_>,
) -> Result<SendReport, VivariumError> {
    if state.room(room_id)?.is_none() {
        return Err(VivariumError::Message(format!(
            "room '{room_id}' is not joined; run vivi chat join first"
        )));
    }
    if draft.body.len() > MAX_BODY_BYTES {
        return Err(VivariumError::Message(format!(
            "body is {} bytes over the {} byte chat cap",
            draft.body.len() - MAX_BODY_BYTES,
            MAX_BODY_BYTES
        )));
    }
    draft.validate()?;
    let (message_id, envelope) = compose_message(draft, identity);
    state.enqueue(room_id, &message_id, &envelope.to_string())?;
    let summary = deliver_all(server, state, identity)?;
    let delivered = summary.sent_ids.iter().any(|id| id == &message_id);
    let queued_reason = if delivered {
        None
    } else {
        Some(
            summary
                .error
                .clone()
                .unwrap_or_else(|| "delivery did not acknowledge this message".into()),
        )
    };
    Ok(SendReport {
        message_id,
        delivered,
        queued_reason,
        summary,
    })
}

/// Deliver every pending outbox row at-least-once. Bearers are minted fresh
/// per room per invocation and never persisted; the pass stops at the first
/// delivery failure (the server dedups Message-IDs on retry).
///
/// # Errors
/// Returns an error only on a state-store failure or undecodable outbox row.
pub fn deliver_all(
    server: &str,
    state: &ChatState,
    identity: &ChatIdentity,
) -> Result<FlushSummary, VivariumError> {
    let mut summary = FlushSummary::empty();
    let mut tokens: HashMap<String, String> = HashMap::new();
    for row in state.pending_outbox()? {
        let Some(room) = state.room(&row.room_id)? else {
            summary.queued += 1;
            summary.error = Some(format!("room '{}' is not joined", row.room_id));
            continue;
        };
        let token = match tokens.get(&room.room_id) {
            Some(token) => token.clone(),
            None => match client::mint_bearer(server, &room.room_id, identity) {
                Ok(token) => {
                    tokens.insert(room.room_id.clone(), token.clone());
                    token
                }
                Err(error) => {
                    summary.queued += 1;
                    summary.error = Some(error.to_string());
                    break;
                }
            },
        };
        match post_row(server, state, identity, &room.label, &row, &token) {
            Ok(duplicate) => {
                summary.sent += 1;
                if duplicate {
                    summary.duplicates += 1;
                }
                summary.sent_ids.push(row.message_id);
            }
            Err(error) => {
                summary.queued += 1;
                summary.error = Some(error.to_string());
                break;
            }
        }
    }
    Ok(summary)
}

/// POST one row and mark it flushed on any ack (201 fresh, 200 duplicate).
fn post_row(
    server: &str,
    state: &ChatState,
    identity: &ChatIdentity,
    label: &str,
    row: &crate::chat::state::PendingMessage,
    token: &str,
) -> Result<bool, VivariumError> {
    let envelope: Value = serde_json::from_str(&row.payload_json)
        .map_err(|_| VivariumError::Message("outbox row holds invalid json".into()))?;
    let mut payload = json!({
        "to": envelope.get("to").cloned().unwrap_or(Value::Null),
        "subject": envelope.get("subject").cloned().unwrap_or(Value::Null),
        "body": envelope.get("body").cloned().unwrap_or(Value::Null),
    });
    for field in ["kind", "in_reply_to", "refs"] {
        if let Some(value) = envelope.get(field) {
            payload[field] = value.clone();
        }
    }
    let ack = client::post_message(
        server,
        &row.room_id,
        token,
        label,
        &identity.short_wire_fingerprint(),
        &row.message_id,
        &payload,
    )?;
    state.mark_flushed(row.id)?;
    Ok(ack.duplicate)
}

/// Poll one room since its stored cursor (from zero with `all`) and advance
/// the stored cursor when `mark_read`.
///
/// # Errors
/// Returns an error when the room is not joined, the bearer mint fails, the
/// poll fails, or the state store fails.
pub fn read_room(
    server: &str,
    state: &ChatState,
    identity: &ChatIdentity,
    room_id: &str,
    mark_read: bool,
    all: bool,
) -> Result<ReadPage, VivariumError> {
    let room = state.room(room_id)?.ok_or_else(|| {
        VivariumError::Message(format!(
            "room '{room_id}' is not joined; run vivi chat join first"
        ))
    })?;
    let since = if all {
        0
    } else {
        state
            .cursor(room_id)?
            .and_then(|cursor| cursor.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let token = client::mint_bearer(server, &room.room_id, identity)?;
    let page = client::poll_messages(
        server,
        &room.room_id,
        &token,
        &room.label,
        &identity.short_wire_fingerprint(),
        since,
        0,
    )?;
    let advanced = !all && mark_read;
    if advanced {
        state.set_cursor(room_id, &page.next_cursor.to_string())?;
    }
    Ok(ReadPage {
        room_id: room.room_id,
        messages: page.messages,
        next_cursor: page.next_cursor,
        advanced,
    })
}

/// Render one message: cursor, timestamp, origin label, short fingerprint.
/// A work item (need/task/want) also shows its kind before the subject;
/// plain chat renders exactly as before.
#[must_use]
pub fn render_message(message: &ChatMessage) -> String {
    let to = message.to.as_deref().unwrap_or("all");
    let kind = message
        .kind
        .as_deref()
        .filter(|kind| !kind.is_empty())
        .map(|kind| format!("[{kind}] "))
        .unwrap_or_default();
    format!(
        "[{}] {} {} <{}> -> {}: {}{}\n  {}",
        message.cursor,
        message.created_at,
        message.from,
        message.fingerprint,
        to,
        kind,
        message.subject,
        message.body
    )
}

#[cfg(test)]
#[path = "outbox_test.rs"]
mod tests;
