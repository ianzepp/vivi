//! Down-sync: file pulled room items into the mailspace.
//!
//! `vivi chat pull` polls a room since its sync cursor and ingests each item
//! as a real mail item under its traveling id, filed by kind (D6) with one
//! `vivi-room` remote binding (room = mailbox, cursor = UID, D4). Ingest is
//! idempotent and never touches local state, so a replay or a resync from
//! cursor 0 is safe. The sync cursor is independent of the read cursor.

use std::collections::BTreeMap;

use crate::chat::client::{self, ChatMessage, PollPage};
use crate::chat::identity::ChatIdentity;
use crate::chat::item::{self, PROVIDER, UIDVALIDITY};
use crate::chat::state::{ChatState, JoinedRoom};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::{
    IngestEvent, IngestOutcome, MessageIngestRequest, RemoteBindingInput, Storage, StoredMessage,
    is_traveling_id,
};

/// What one pull did, for the operator.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PullReport {
    pub room_id: String,
    pub fetched: usize,
    /// New items filed, by local role.
    pub filed: BTreeMap<String, usize>,
    /// Evicted items brought back by a resync.
    pub restored: usize,
    /// Items already present; left untouched.
    pub existing: usize,
    /// Direct items addressed to another seat (the server shows the whole
    /// room to every seat; this client only files what is for it).
    pub not_addressed: usize,
    /// Items that could not be rendered (bad timestamp, cursor out of range).
    pub malformed: usize,
    /// Items filed under a store-local id because their wire id is not a
    /// traveling id (sent before phase 02, or by another client).
    pub local_ids: usize,
    /// Reply links recorded against a parent already in the mailspace.
    pub linked: usize,
    /// Ids this mailspace already holds with different bytes. The newcomer
    /// is refused, never swapped in, and the pull carries on.
    pub conflicts: Vec<String>,
    pub next_cursor: u64,
}

impl PullReport {
    /// One-line operator summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let filed: usize = self.filed.values().sum();
        let mut parts = vec![format!("{filed} new")];
        for (role, count) in &self.filed {
            parts.push(format!("{count} {role}"));
        }
        parts.push(format!("{} already present", self.existing));
        for (count, what) in [
            (self.restored, "restored"),
            (self.not_addressed, "for other seats"),
            (self.malformed, "malformed (skipped)"),
            (self.local_ids, "under store-local ids"),
            (self.linked, "reply link(s)"),
            (
                self.conflicts.len(),
                "refused (id already held with different content)",
            ),
        ] {
            if count > 0 {
                parts.push(format!("{count} {what}"));
            }
        }
        format!(
            "room {}: {} fetched, {}, cursor {}",
            self.room_id,
            self.fetched,
            parts.join(", "),
            self.next_cursor
        )
    }
}

/// The local role that receives room items, from `[chat] recipient`.
///
/// # Errors
/// Returns an error when none is configured (down-sync is opt-in) or the
/// configured name is not a known local role.
pub fn resolve_recipient(mailspace: &Mailspace) -> Result<String, VivariumError> {
    let configured = mailspace
        .config
        .chat
        .as_ref()
        .and_then(|chat| chat.recipient.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            VivariumError::Config(
                "no recipient configured; set [chat] recipient = \"<role>\" in mailspace.toml \
                 (the local role that receives room items)"
                    .into(),
            )
        })?;
    mailspace.resolve_identity(configured)
}

/// Poll `room` and ingest what arrives. `from_zero` replays the whole room
/// (resync); otherwise the poll starts at the stored sync cursor. The cursor
/// advances only after every item of the page is handled, so a failure
/// replays the page and ingest absorbs the repeats.
///
/// # Errors
/// Returns an error when the bearer mint, poll, storage, or cursor write
/// fails.
pub fn pull(
    server: &str,
    mailspace: &Mailspace,
    state: &ChatState,
    identity: &ChatIdentity,
    room: &JoinedRoom,
    from_zero: bool,
) -> Result<PullReport, VivariumError> {
    let recipient = resolve_recipient(mailspace)?;
    let fetch = |since: u64| -> Result<PollPage, VivariumError> {
        let token = client::mint_bearer(server, &room.room_id, identity)?;
        client::poll_messages(
            server,
            &room.room_id,
            &token,
            &room.label,
            &identity.short_wire_fingerprint(),
            since,
            0,
        )
    };
    pull_with(mailspace, state, room, &recipient, from_zero, fetch)
}

/// [`pull`] with the poll injected, so the cursor and ingest behavior is
/// testable without a server.
///
/// # Errors
/// As [`pull`].
pub fn pull_with(
    mailspace: &Mailspace,
    state: &ChatState,
    room: &JoinedRoom,
    recipient: &str,
    from_zero: bool,
    fetch: impl FnOnce(u64) -> Result<PollPage, VivariumError>,
) -> Result<PullReport, VivariumError> {
    let since = if from_zero {
        0
    } else {
        state
            .sync_cursor(&room.room_id)?
            .map_or(0, |cursor| cursor.parse().unwrap_or(0))
    };
    let page = fetch(since)?;
    let mut report = ingest_page(mailspace, room, recipient, &page.messages)?;
    report.next_cursor = page.next_cursor;
    state.set_sync_cursor(&room.room_id, &page.next_cursor.to_string())?;
    Ok(report)
}

/// Ingest one page of room records for `recipient`.
///
/// # Errors
/// Returns an error on a storage failure; skipped records are counted, not
/// errors.
pub fn ingest_page(
    mailspace: &Mailspace,
    room: &JoinedRoom,
    recipient: &str,
    messages: &[ChatMessage],
) -> Result<PullReport, VivariumError> {
    let mut storage = mailspace.storage()?;
    let mut report = PullReport {
        room_id: room.room_id.clone(),
        fetched: messages.len(),
        ..PullReport::default()
    };
    for message in messages {
        ingest_one(
            mailspace,
            &mut storage,
            room,
            recipient,
            message,
            &mut report,
        )?;
    }
    Ok(report)
}

fn ingest_one(
    mailspace: &Mailspace,
    storage: &mut Storage,
    room: &JoinedRoom,
    recipient: &str,
    message: &ChatMessage,
    report: &mut PullReport,
) -> Result<(), VivariumError> {
    let own = message.from == room.label;
    if !own && message.to.as_deref().is_some_and(|to| to != room.label) {
        report.not_addressed += 1;
        return Ok(());
    }
    let (Ok(eml), Ok(uid)) = (
        item::render_eml(&room.room_id, message),
        u32::try_from(message.cursor),
    ) else {
        report.malformed += 1;
        return Ok(());
    };
    let role = item::role_for(message.kind.as_deref(), own);
    let traveling = is_traveling_id(&message.message_id);
    let request = ingest_request(room, recipient, message, role, uid);
    let note = format!(
        "room {} seat {} cursor {}",
        room.room_id, message.from, message.cursor
    );
    let event = IngestEvent {
        command: "chat pull",
        event_type: if own {
            "sent_copy_created"
        } else {
            "delivered"
        },
        subject: &message.subject,
        note: Some(&note),
    };
    let (outcome, stored) = storage.ingest_if_absent(&request, eml.as_bytes(), Some(&event))?;
    match outcome {
        IngestOutcome::Inserted => {
            *report.filed.entry(role.to_string()).or_default() += 1;
            if !traveling {
                report.local_ids += 1;
            }
            attach_backlog(mailspace, storage, message, own, &stored)?;
        }
        IngestOutcome::Restored => report.restored += 1,
        IngestOutcome::Existing => report.existing += 1,
        IngestOutcome::Conflict => {
            report.conflicts.push(stored.message_id);
            return Ok(());
        }
    }
    if link_reply(storage, message, &stored)? {
        report.linked += 1;
    }
    Ok(())
}

/// The ingest request for one room item. A traveling wire id is presented as
/// the message id; anything else (legacy ids, other clients) falls through to
/// the deterministic remote-bound mint, so the same item always maps to the
/// same local id.
fn ingest_request(
    room: &JoinedRoom,
    recipient: &str,
    message: &ChatMessage,
    role: &str,
    uid: u32,
) -> MessageIngestRequest {
    MessageIngestRequest {
        account: recipient.to_string(),
        local_role: role.to_string(),
        read_state: message.from == room.label,
        starred: false,
        message_id_hint: None,
        presented_id: is_traveling_id(&message.message_id).then(|| message.message_id.clone()),
        seed_hint: format!("vivi-room\0{}\0{}", room.room_id, message.cursor),
        remote: Some(RemoteBindingInput {
            account: recipient.to_string(),
            provider: PROVIDER.to_string(),
            remote_mailbox: room.room_id.clone(),
            remote_uid: uid,
            remote_uidvalidity: UIDVALIDITY,
        }),
    }
}

/// A newly filed need/task/want is backlog work like a locally sent one.
fn attach_backlog(
    mailspace: &Mailspace,
    storage: &Storage,
    message: &ChatMessage,
    own: bool,
    stored: &StoredMessage,
) -> Result<(), VivariumError> {
    let Some(kind) = item::work_kind(message.kind.as_deref()) else {
        return Ok(());
    };
    if own {
        return Ok(());
    }
    let handle = storage.display_handle(&stored.message_id)?;
    mailspace.backlog_attach(kind, &handle, &message.subject, &[])
}

/// Record the reply relation when the parent is a live item in this
/// mailspace. A parent this store has not seen stays a dangling reference
/// (D3); a resync links it once the parent arrives.
fn link_reply(
    storage: &Storage,
    message: &ChatMessage,
    child: &StoredMessage,
) -> Result<bool, VivariumError> {
    let Some(parent_id) = message
        .in_reply_to
        .as_deref()
        .filter(|id| is_traveling_id(id))
    else {
        return Ok(false);
    };
    let Ok(parent_id) = storage.resolve_message_token(parent_id) else {
        return Ok(false);
    };
    let Some(parent) = storage.message_by_id(&parent_id)? else {
        return Ok(false);
    };
    if parent.content_id == child.content_id {
        return Ok(false);
    }
    storage.link_mailspace_content(&child.content_id, &parent.content_id, "captured")?;
    Ok(true)
}

#[cfg(test)]
#[path = "sync_test.rs"]
mod tests;
