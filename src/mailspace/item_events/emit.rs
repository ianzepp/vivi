//! Writing an item-state event: the owner's `done` and `reopen`.

use std::collections::{BTreeMap, BTreeSet};

use mail_parser::{Address, Message, MessageParser};

use super::{
    EVENT_AT_HEADER, EVENT_HEADER, EVENT_OF_HEADER, EVENT_PREV_HEADER, EventLetter, HeldItem,
    ItemEventKind, chain_heads, dated_within_clock, held_item, role_move,
};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::mailspace::Recipient;
use crate::mailspace::delivery::add_header;
use crate::mailspace::reply::{add_reply_headers, reply_subject};
use crate::message::{ComposeDraft, build_compose_draft};
use crate::storage::{
    ItemEventWrite, MailspaceEventInput, MessageIngestRequest, RoleMove, Storage, StoredMessageView,
};

/// One event an owner writes.
pub(super) struct EmitRequest<'a> {
    /// The acting role: the item's owner.
    pub(super) identity: &'a str,
    /// The row the actor pointed at.
    pub(super) before: &'a StoredMessageView,
    pub(super) kind: ItemEventKind,
    /// The note: the letter's body, and the lifecycle row's note.
    pub(super) note: Option<&'a str>,
    /// The lifecycle command this action is logged as (`task done`).
    pub(super) command: &'a str,
    /// Why, when no person acted (`join`): the lifecycle row's note.
    pub(super) via: Option<&'a str>,
    /// Extra letter headers the event carries (a close's verdict and repo
    /// tips), beyond the event chain's own.
    pub(super) headers: Vec<(String, String)>,
}

impl EmitRequest<'_> {
    pub(super) fn log_note(&self) -> Option<String> {
        self.note
            .map(str::to_string)
            .or_else(|| self.via.map(|via| format!("via={via}")))
    }
}

/// The event kind a folder move is, if it is one: a close of open work, a
/// reopen of closed work, or a want's promotion into `needs`. A reopen may
/// point at the owner's `sent` record of a send whose delivered copies are
/// closed; the caller checks that. A close may point at a copy an assignee
/// already reported done, which the owner still has to close; the caller
/// checks that too.
fn event_kind_for_move(from: &str, to: &str) -> Option<ItemEventKind> {
    match (from, to) {
        ("tasks" | "needs" | "wants" | "sent" | "done", "done") => Some(ItemEventKind::Done),
        ("wants", "needs") => Some(ItemEventKind::Promoted),
        ("done" | "sent", "tasks" | "needs" | "wants") => Some(ItemEventKind::Reopened),
        _ => None,
    }
}

/// A composed event letter and the local roles that get a delivered copy.
struct EventLetterDraft {
    data: Vec<u8>,
    local: BTreeSet<String>,
}

impl Mailspace {
    /// A close's verdict metadata as letter headers: `verdict` becomes
    /// `X-Vivi-Verdict`, and each `repo:<name>` becomes
    /// `X-Vivi-Repo: <name>=<tip>`. A close that travels as an event letter
    /// carries them to every mailspace holding the item.
    pub(in crate::mailspace) fn verdict_headers(
        metadata: &BTreeMap<String, String>,
    ) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if let Some(verdict) = metadata.get("verdict") {
            headers.push(("X-Vivi-Verdict".into(), verdict.clone()));
        }
        for (key, tip) in metadata {
            if let Some(repo) = key.strip_prefix("repo:") {
                headers.push(("X-Vivi-Repo".into(), format!("{repo}={tip}")));
            }
        }
        headers
    }

    /// Write the event letter for a `done`, `reopen` or `promote` move by a
    /// role that may close the item (its author, or a mind or operator) and
    /// apply it; the result is the event letter's handle. `Ok(None)` when the
    /// move is not an event: not a close, reopen or promotion, not work, or the
    /// actor may not close the item (an assignee's move is a local report).
    /// Needs the applied event's join rule completes are pushed onto `joined`
    /// as the mailspace shows them.
    pub(in crate::mailspace) fn try_emit_for_move(
        &self,
        storage: &Storage,
        identity: &str,
        before: &StoredMessageView,
        role: &str,
        note: Option<&str>,
        command: &str,
        extras: &[(String, String)],
        joined: &mut Vec<String>,
    ) -> Result<Option<String>, VivariumError> {
        let Some(kind) = event_kind_for_move(&before.local_role, role) else {
            return Ok(None);
        };
        let Some(item) = held_item(storage, &before.content_id)? else {
            return Ok(None);
        };
        let head = self.item_head(storage, &item)?;
        if !self.may_close(&item, identity) {
            return refuse_assignee_reopen(before, kind, head, &item);
        }
        if kind == ItemEventKind::Done
            && before.local_role == "done"
            && head == Some(ItemEventKind::Done)
        {
            // Already closed by an event: the move is a no-op.
            return Ok(None);
        }
        if kind == ItemEventKind::Reopened
            && before.local_role == "sent"
            && head != Some(ItemEventKind::Done)
            && self
                .plan_item_moves(storage, &item, kind, false)?
                .is_empty()
        {
            // Nothing is closed: reopening an open sent copy would land a
            // second owner here, which the local move refuses.
            return Ok(None);
        }
        let request = EmitRequest {
            identity,
            before,
            kind,
            note,
            command,
            via: None,
            headers: extras.to_vec(),
        };
        self.emit_item_event(storage, &item, &request, joined)
            .map(Some)
    }

    /// Settle a need's copies the way its owner closing or reopening it
    /// would: as an event letter. `Ok(false)` when this mailspace does not
    /// own the need (the caller moves the copies locally). Needs the settle's
    /// event application joins are pushed onto `joined`.
    pub(in crate::mailspace) fn settle_need_by_event(
        &self,
        storage: &Storage,
        view: &StoredMessageView,
        to_done: bool,
        command: &str,
        via: &str,
        joined: &mut Vec<String>,
    ) -> Result<bool, VivariumError> {
        let Some(item) = held_item(storage, &view.content_id)? else {
            return Ok(false);
        };
        let Some(identity) = self.item_author(&item) else {
            return Ok(false);
        };
        let kind = if to_done {
            ItemEventKind::Done
        } else {
            ItemEventKind::Reopened
        };
        if self
            .plan_item_moves(storage, &item, kind, false)?
            .is_empty()
        {
            return Ok(true);
        }
        let request = EmitRequest {
            identity: &identity,
            before: view,
            kind,
            note: None,
            command,
            via: Some(via),
            headers: Vec::new(),
        };
        self.emit_item_event(storage, &item, &request, joined)?;
        Ok(true)
    }

    /// The row an identity acts on when it names work it does not hold: any
    /// row of the item here, when the move closes or reopens it and the
    /// identity may close it (its author or holder of the sent copy, or a
    /// mind or operator, whichever mailspace authored the item). `Ok(None)`
    /// otherwise; the caller reports who holds the row.
    pub(in crate::mailspace) fn row_for_item_actor(
        &self,
        storage: &Storage,
        identity: &str,
        token: &str,
        role: &str,
    ) -> Result<Option<StoredMessageView>, VivariumError> {
        let Ok(resolved) = storage.resolve_message_token(token) else {
            return Ok(None);
        };
        let Some(row) = storage.message_by_id(&resolved)? else {
            return Ok(None);
        };
        if event_kind_for_move(&row.local_role, role).is_none() {
            return Ok(None);
        }
        let Some(item) = held_item(storage, &row.content_id)? else {
            return Ok(None);
        };
        if !self.may_close(&item, identity) {
            return Ok(None);
        }
        if row.local_role == "done"
            && role == "done"
            && self.item_head(storage, &item)? == Some(ItemEventKind::Done)
        {
            return Err(VivariumError::Message(format!(
                "{token} is already closed by its owner; reopen it to change it"
            )));
        }
        Ok(Some(row))
    }

    /// Whether moving `before` to `role` is a close or reopen of work: an
    /// event when the actor may close it, a report when not.
    pub(in crate::mailspace) fn is_work_close(
        &self,
        storage: &Storage,
        before: &StoredMessageView,
        role: &str,
    ) -> Result<bool, VivariumError> {
        if event_kind_for_move(&before.local_role, role).is_none() {
            return Ok(false);
        }
        Ok(held_item(storage, &before.content_id)?.is_some())
    }

    /// The refusal for naming work another role holds when the actor may not
    /// close it: who holds it, the command that reports on it, and who may
    /// close it. Falls back to the plain holder error for anything else.
    pub(in crate::mailspace) fn foreign_move_error(
        &self,
        storage: &Storage,
        identity: &str,
        token: &str,
        role: &str,
        command: &str,
    ) -> VivariumError {
        let error = self.cross_identity_error(storage, token);
        let Ok(resolved) = storage.resolve_message_token(token) else {
            return error;
        };
        let Ok(Some(row)) = storage.message_by_id(&resolved) else {
            return error;
        };
        let Some(holder) = self.identity_owning_account(&row.account) else {
            return error;
        };
        if event_kind_for_move(&row.local_role, role).is_none() {
            return error;
        }
        let Ok(Some(item)) = held_item(storage, &row.content_id) else {
            return error;
        };
        let author = self
            .item_author(&item)
            .map_or_else(|| item.from_addr.clone(), |name| format!("'{name}'"));
        let report = if command == "item move" {
            format!("--for {holder}")
        } else {
            format!("--for {holder} (vivi {command} {token} --for {holder})")
        };
        let why = format!("'{identity}' is neither");
        VivariumError::Message(format!(
            "message '{token}' is held by identity '{holder}'; to report on it retry with \
             {report}, which moves only that copy. Closing or reopening it for everyone is \
             for its author {author}, a mind-kind role or an operator-kind role; {why}"
        ))
    }

    fn emit_item_event(
        &self,
        storage: &Storage,
        item: &HeldItem,
        request: &EmitRequest<'_>,
        joined: &mut Vec<String>,
    ) -> Result<String, VivariumError> {
        let chain = self.item_chain(storage, item)?;
        let prevs: Vec<String> = chain_heads(&chain.events)
            .iter()
            .map(|head| head.id.clone())
            .collect();
        let at = event_at(super::clock_micros(), &chain.events);
        let letter = self.compose_event_letter(item, request, &prevs, &at)?;
        self.check_letter_size(letter.data.len())?;
        let rows = event_rows(request.identity, &letter.local, &item.content_id);
        let moves = self.event_moves(storage, item, request)?;
        let written = storage.apply_item_event(&ItemEventWrite {
            reply_requests: &rows,
            reply_data: Some(&letter.data),
            parent_content_id: &item.content_id,
            moves: &moves,
        })?;
        self.sync_item_nodes(storage, item, request.kind, "via=lifecycle", joined)?;
        // The last row is the actor's own sent copy of the letter.
        let event = written
            .last()
            .ok_or_else(|| VivariumError::Message("the event letter was not stored".into()))?;
        storage.display_handle(&event.message_id)
    }

    /// The folder moves the event makes, with the actor's own lifecycle row:
    /// logged against the row they pointed at even when that row stays put
    /// (the sender's record of a local send).
    fn event_moves(
        &self,
        storage: &Storage,
        item: &HeldItem,
        request: &EmitRequest<'_>,
    ) -> Result<Vec<RoleMove>, VivariumError> {
        let mut moves: Vec<RoleMove> = self
            .plan_item_moves(storage, item, request.kind, false)?
            .iter()
            .map(|planned| role_move(item, planned, request.kind, Some(request)))
            .collect();
        let before = request.before;
        if !moves
            .iter()
            .any(|step| step.message_id == before.message_id)
        {
            moves.push(RoleMove {
                account: before.account.clone(),
                message_id: before.message_id.clone(),
                to_role: None,
                event: MailspaceEventInput {
                    command: request.command.into(),
                    event_type: "moved".into(),
                    actor_identity: Some(request.identity.into()),
                    account: before.account.clone(),
                    message_id: before.message_id.clone(),
                    content_id: before.content_id.clone(),
                    from_role: Some(before.local_role.clone()),
                    to_role: Some(target_role(item, request.kind).into()),
                    from_identity: Some(request.identity.into()),
                    to_identity: Some(request.identity.into()),
                    subject: before.subject.clone(),
                    note: request.log_note(),
                },
            });
        }
        Ok(moves)
    }

    fn compose_event_letter(
        &self,
        item: &HeldItem,
        request: &EmitRequest<'_>,
        prevs: &[String],
        at: &str,
    ) -> Result<EventLetterDraft, VivariumError> {
        let parsed = MessageParser::default()
            .parse(&item.bytes)
            .ok_or_else(|| VivariumError::Parse("failed to parse the item".into()))?;
        let item_id = parsed
            .message_id()
            .ok_or_else(|| VivariumError::Message("the item has no Message-ID".into()))?;
        let from = self.address_for(request.identity);
        let (mut to, cc) = self.event_addresses(&parsed, request.identity, &from);
        let local = self.local_event_recipients(&to, &cc, request.identity);
        if to.is_empty() && cc.is_empty() {
            // Nobody else is on the item: the letter is the owner's own
            // record, and a restored checkout of the mailspace reads it back.
            to.push(from.clone());
        }
        let body = request
            .note
            .map_or_else(|| request.kind.header().to_string(), str::to_string);
        let mut data = build_compose_draft(&ComposeDraft {
            from,
            to,
            cc,
            bcc: Vec::new(),
            subject: reply_subject(parsed.subject().unwrap_or("(no subject)")),
            body,
            html_body: None,
        })?;
        data = add_header(&data, "X-Vivi-Kind", "mail")?;
        data = add_header(&data, EVENT_HEADER, request.kind.header())?;
        data = add_header(&data, EVENT_OF_HEADER, &format!("<{item_id}>"))?;
        data = add_header(&data, EVENT_AT_HEADER, at)?;
        for prev in prevs {
            data = add_header(&data, EVENT_PREV_HEADER, &format!("<{prev}>"))?;
        }
        for (name, value) in &request.headers {
            data = add_header(&data, name, value)?;
        }
        data = add_reply_headers(&data, &item.bytes)?;
        Ok(EventLetterDraft {
            data: data.into_bytes(),
            local,
        })
    }

    /// The item's own From, To and Cc, minus the sender: the From first in
    /// To, so an event written outside the author's mailspace reaches the
    /// author, then the item's To, with the item's Cc as Cc and no
    /// duplicates anywhere.
    pub(super) fn event_addresses(
        &self,
        parsed: &Message<'_>,
        identity: &str,
        from: &str,
    ) -> (Vec<String>, Vec<String>) {
        let mut seen: Vec<String> = Vec::new();
        let mut keep = |list: Option<&Address<'_>>| -> Vec<String> {
            let mut kept: Vec<String> = Vec::new();
            for address in list.into_iter().flat_map(Address::iter) {
                let Some(value) = address.address.as_deref() else {
                    continue;
                };
                let is_sender = value.eq_ignore_ascii_case(from)
                    || matches!(
                        self.classify_recipient(value),
                        Ok(Recipient::Local(id)) if id == identity
                    );
                if !is_sender && !seen.iter().any(|other| other.eq_ignore_ascii_case(value)) {
                    seen.push(value.to_string());
                    kept.push(value.to_string());
                }
            }
            kept
        };
        let mut to = keep(parsed.from());
        to.extend(keep(parsed.to()));
        let cc = keep(parsed.cc());
        (to, cc)
    }

    /// The roles of this mailspace the letter is delivered to.
    pub(super) fn local_event_recipients(
        &self,
        to: &[String],
        cc: &[String],
        identity: &str,
    ) -> BTreeSet<String> {
        to.iter()
            .chain(cc)
            .filter_map(|address| match self.classify_recipient(address) {
                Ok(Recipient::Local(id)) if id != identity => Some(id),
                _ => None,
            })
            .collect()
    }
}

/// The `X-Vivi-Event-At` value for a new event: the clock, or one
/// microsecond past the newest counted event this mailspace holds for the
/// item, whichever is later — a writer never stamps an event older than
/// one it has seen, and never below an instant a reader will not count.
fn event_at(now_micros: i64, events: &[EventLetter]) -> String {
    let floor = events
        .iter()
        .filter(|event| dated_within_clock(event.at, now_micros))
        .map(|event| event.at.saturating_add(1))
        .max()
        .unwrap_or(now_micros);
    stamp_micros(now_micros.max(floor))
}

/// An instant in Unix microseconds as RFC 3339 UTC with microseconds, as
/// long as a reader parses it back: past 9999 the year overflows four
/// digits (`+10000-...`), so the clock is stamped instead.
pub(super) fn stamp_micros(micros: i64) -> String {
    let stamp = stamped(micros);
    if chrono::DateTime::parse_from_rfc3339(&stamp).is_ok() {
        return stamp;
    }
    stamped(chrono::Utc::now().timestamp_micros())
}

/// An instant as RFC 3339 UTC with microseconds; the clock when the instant
/// is outside what `chrono` can hold.
fn stamped(micros: i64) -> String {
    chrono::DateTime::from_timestamp_micros(micros)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// An assignee cannot reopen what its owner closed: the owner's close is
/// the latest letter, and a local reopen would be undone by the next pull
/// or audit. Any other non-owner move is a local report (`Ok(None)`).
fn refuse_assignee_reopen(
    before: &StoredMessageView,
    kind: ItemEventKind,
    head: Option<ItemEventKind>,
    item: &HeldItem,
) -> Result<Option<String>, VivariumError> {
    if kind == ItemEventKind::Reopened
        && before.local_role == "done"
        && head == Some(ItemEventKind::Done)
    {
        return Err(VivariumError::Message(format!(
            "{} was closed by its owner {}; only the owner can reopen it",
            before.handle, item.from_addr
        )));
    }
    Ok(None)
}

/// The folder the actor's row heads to.
fn target_role(item: &HeldItem, kind: ItemEventKind) -> &'static str {
    match (kind, item.kind) {
        (ItemEventKind::Promoted, _) => "needs",
        (ItemEventKind::Done, _) => "done",
        (ItemEventKind::Reopened, "need") => "needs",
        (ItemEventKind::Reopened, "want") => "wants",
        (ItemEventKind::Reopened, _) => "tasks",
    }
}

/// The rows an event letter takes here: a read delivered copy for each local
/// recipient (events do not pile up in inboxes) and the owner's sent copy.
pub(super) fn event_rows(
    identity: &str,
    local: &BTreeSet<String>,
    item_content_id: &str,
) -> Vec<MessageIngestRequest> {
    let mut rows: Vec<MessageIngestRequest> = local
        .iter()
        .map(|recipient| MessageIngestRequest {
            account: recipient.clone(),
            local_role: "inbox".into(),
            read_state: true,
            starred: false,
            message_id_hint: None,
            presented_id: None,
            seed_hint: format!("item-event\0{item_content_id}\0{recipient}"),
            remote: None,
        })
        .collect();
    rows.push(MessageIngestRequest {
        account: identity.into(),
        local_role: "sent".into(),
        read_state: true,
        starred: false,
        message_id_hint: None,
        presented_id: None,
        seed_hint: format!("item-event-sent\0{item_content_id}"),
        remote: None,
    });
    rows
}
