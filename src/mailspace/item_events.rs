//! State as letters.
//!
//! Closing or reopening a need, task, or want is a small reply letter on the
//! item, written by the item's owner (the role that authored it):
//!
//! ```text
//! In-Reply-To: <item Message-ID>
//! X-Vivi-Event: done | reopened
//! X-Vivi-Event-Of: <item Message-ID>
//! X-Vivi-Event-Prev: <Message-ID of the event this one follows>   (repeatable)
//! ```
//!
//! The state of an item is a pure function of the letters held. Order is
//! the clock, never the server: every event carries an instant — its
//! `X-Vivi-Event-At` (RFC 3339), else its `Date` — and the newest event is
//! the state. On equal instants the deeper chain wins (a direct successor
//! names the other as its `Prev`, so it carries strictly more history),
//! else the larger normalized `Message-ID`, so every checkout that holds
//! the same letters agrees whatever order they arrived in. A chain no
//! longer beats a later clock: a later instant wins even over an event that
//! names it as `Prev`, and a fork is settled the same way on every checkout
//! alike. An instant more than ten minutes past the receiver's clock counts
//! for nothing at all: no letter can lock an item from the future.
//!
//! An event counts when its From domain is a prefix domain (`<8 hex>.local`),
//! whoever authored the item: the server stamps every record with the
//! sender's registered prefix and the pull refuses a prefix-form From it did
//! not stamp, so such a From names a real member, and no member owns an
//! item's state. A bare `local` or a legacy name domain is a claim anyone
//! can make: it is filed as plain mail and never counts.
//!
//! One function projects a head onto the rows and the backlog node. The owner
//! calls it when it writes an event; every other route (pull, fetch, resync,
//! reference follow, `graph audit --repair`) calls it for the letters it
//! sees, so a store with stale rows converges. Replies are found through the
//! reply links, so an event that arrives before its item is applied when the
//! item arrives and the link is made.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

use mail_parser::{HeaderValue, MessageParser};
use serde::Serialize;

use super::holding::{first_bare_address, is_foreign_account};
use super::kind::{effective_kind, message_kind};
use super::{Mailspace, prefix_of_domain};
use crate::error::VivariumError;
use crate::message::normalize_message_id;
use crate::storage::{ItemEventWrite, MailspaceEventInput, RoleMove, Storage, StoredMessageView};

mod emit;
mod property;
mod property_emit;

pub(crate) use property::{PropertyKind, parse_property_event};

pub(crate) const EVENT_HEADER: &str = "X-Vivi-Event";
const EVENT_OF_HEADER: &str = "X-Vivi-Event-Of";
const EVENT_PREV_HEADER: &str = "X-Vivi-Event-Prev";
const EVENT_AT_HEADER: &str = "X-Vivi-Event-At";

/// What an item-state event says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemEventKind {
    Done,
    Reopened,
}

impl ItemEventKind {
    /// The `X-Vivi-Event` value.
    #[must_use]
    pub fn header(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Reopened => "reopened",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "done" => Some(Self::Done),
            "reopened" => Some(Self::Reopened),
            _ => None,
        }
    }

    /// The lifecycle-log command for an item of `kind`.
    fn command(self, kind: &str) -> String {
        match self {
            Self::Done => format!("{kind} done"),
            Self::Reopened => format!("{kind} reopen"),
        }
    }
}

/// An item-state event letter, read from its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EventLetter {
    pub(crate) kind: ItemEventKind,
    /// Normalized `Message-ID` of the event letter itself.
    pub(crate) id: String,
    /// Normalized `Message-ID` of the item.
    pub(crate) of: String,
    /// Normalized `Message-ID`s of the events this one follows.
    pub(crate) prev: Vec<String>,
    pub(crate) from_domain: String,
    /// The `Date` header as a Unix time; 0 when absent.
    pub(crate) date: i64,
    /// When the event happened: `X-Vivi-Event-At` (RFC 3339) in Unix
    /// microseconds when present and parseable, else the `Date` header in
    /// whole seconds × 1 000 000.
    pub(crate) at: i64,
}

/// Read an item-state event from a letter's bytes. A letter counts only when
/// it names the item both as `Event-Of` and as the message it replies to.
pub(crate) fn parse_event(bytes: &[u8]) -> Option<EventLetter> {
    let parsed = MessageParser::default().parse(bytes)?;
    let mut kind = None;
    let mut of = None;
    let mut at = None;
    let mut prev: Vec<String> = Vec::new();
    for header in parsed.headers() {
        let HeaderValue::Text(text) = header.value() else {
            continue;
        };
        let name = header.name();
        if name.eq_ignore_ascii_case(EVENT_HEADER) {
            kind = ItemEventKind::parse(text);
        } else if name.eq_ignore_ascii_case(EVENT_OF_HEADER) {
            of = normalize_message_id(text);
        } else if name.eq_ignore_ascii_case(EVENT_AT_HEADER) {
            at = rfc3339_micros(text);
        } else if name.eq_ignore_ascii_case(EVENT_PREV_HEADER)
            && let Some(id) = normalize_message_id(text)
            && !prev.contains(&id)
        {
            prev.push(id);
        }
    }
    let of = of?;
    let replied = parsed
        .in_reply_to()
        .as_text()
        .and_then(normalize_message_id);
    if replied.as_deref() != Some(of.as_str()) {
        return None;
    }
    Some(EventLetter {
        kind: kind?,
        id: parsed.message_id().and_then(normalize_message_id)?,
        of,
        prev,
        from_domain: from_domain_of(&parsed)?,
        date: parsed.date().map_or(0, mail_parser::DateTime::to_timestamp),
        at: at.unwrap_or_else(|| date_micros(&parsed)),
    })
}

/// An `X-Vivi-Event-At` value (RFC 3339) as Unix microseconds.
fn rfc3339_micros(value: &str) -> Option<i64> {
    let Ok(time) = chrono::DateTime::parse_from_rfc3339(value.trim()) else {
        return None;
    };
    Some(time.timestamp_micros())
}

/// The `Date` header of a parsed letter in whole seconds × 1 000 000: the
/// `at` of an event with no parseable stamp.
fn date_micros(parsed: &mail_parser::Message<'_>) -> i64 {
    parsed.date().map_or(0, mail_parser::DateTime::to_timestamp) * 1_000_000
}

fn from_domain_of(parsed: &mail_parser::Message<'_>) -> Option<String> {
    let address = parsed.from()?.first()?.address.as_deref()?;
    let (_, domain) = address.rsplit_once('@')?;
    Some(domain.trim().to_ascii_lowercase())
}

/// The current state of an item and the event that says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemEventState {
    /// `done` or `reopened`.
    pub state: String,
    /// The head event's `Message-ID`.
    pub event: String,
    /// How many heads the chain has; more than one is a fork.
    pub heads: usize,
}

/// What settling a set of arrived letters did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EventTally {
    /// Items whose rows or node moved to match their newest event.
    pub applied: usize,
    /// Events that were not applied: from a domain that is not a member's
    /// prefix domain, dated past the clock, or for an item sealed by absorb.
    pub ignored: usize,
}

/// A work item held here: its rows and the facts events are judged by.
pub(super) struct HeldItem {
    pub(super) content_id: String,
    pub(super) rfc_id: String,
    pub(super) from_domain: String,
    pub(super) from_addr: String,
    /// `need`, `task`, or `want`.
    pub(super) kind: &'static str,
    pub(super) rows: Vec<StoredMessageView>,
    pub(super) bytes: Vec<u8>,
}

/// The held work item behind `content_id`, if it is one.
pub(super) fn held_item(
    storage: &Storage,
    content_id: &str,
) -> Result<Option<HeldItem>, VivariumError> {
    let rows = storage.live_messages_for_content(content_id)?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let Ok(bytes) = storage.read_listed_blob(&first.blob_relpath) else {
        return Ok(None);
    };
    let Some(kind) = work_kind(storage, &bytes, &rows)? else {
        return Ok(None);
    };
    let Some(parsed) = MessageParser::default().parse(&bytes) else {
        return Ok(None);
    };
    let (Some(rfc_id), Some(from_domain)) = (
        parsed.message_id().and_then(normalize_message_id),
        from_domain_of(&parsed),
    ) else {
        return Ok(None);
    };
    let from_addr = first_bare_address(&first.from_addr).unwrap_or_default();
    Ok(Some(HeldItem {
        content_id: content_id.to_string(),
        rfc_id,
        from_domain,
        from_addr,
        kind,
        rows,
        bytes,
    }))
}

/// `need`, `task` or `want`: what the letter's kind header says, else what
/// a row's folder or its close event implies (a letter filed from an
/// envelope kind alone carries no header).
fn work_kind(
    storage: &Storage,
    bytes: &[u8],
    rows: &[StoredMessageView],
) -> Result<Option<&'static str>, VivariumError> {
    let named = |kind: Option<String>| match kind.as_deref() {
        Some("need") => Some("need"),
        Some("task") => Some("task"),
        Some("want") => Some("want"),
        _ => None,
    };
    if let Some(kind) = named(message_kind(bytes)) {
        return Ok(Some(kind));
    }
    for row in rows {
        let events = storage.list_mailspace_events(&row.message_id)?;
        if let Some(kind) = named(effective_kind(&row.local_role, bytes, &events)) {
            return Ok(Some(kind));
        }
    }
    Ok(None)
}

/// The events of one item that count here, and how many named it from a
/// domain that speaks for no member or dated it past the clock.
struct Chain {
    events: Vec<EventLetter>,
    unauthorized: usize,
}

/// Every event head of the chain, oldest to newest by the total order
/// [`newer`] keeps: how wide a fork is, and the events the next letter
/// names as `Prev`. The state is the newest event by that order, not the
/// last head. A cycle (which no honest writer makes) has no head, so every
/// event is a candidate then.
fn chain_heads(events: &[EventLetter]) -> Vec<&EventLetter> {
    let named: HashSet<&str> = events
        .iter()
        .flat_map(|event| event.prev.iter().map(String::as_str))
        .collect();
    let depths = event_depths(events);
    let mut heads: Vec<usize> = (0..events.len())
        .filter(|&index| !named.contains(events[index].id.as_str()))
        .collect();
    if heads.is_empty() {
        heads = (0..events.len()).collect();
    }
    heads.sort_by_key(|&index| (events[index].at, depths[index], &events[index].id));
    heads.into_iter().map(|index| &events[index]).collect()
}

/// How `a` stands against `b` in age: the larger `at` wins, then the deeper
/// chain (a direct successor has strictly more history than the event it
/// names), then the larger normalized `Message-ID`. A total order, so
/// [`newest_event`] picks the same event for every ordering of the same
/// letters. A later clock still beats the chain: `a` stays newer even when
/// `b` names it as `Prev`.
fn newer(a: &EventLetter, a_depth: usize, b: &EventLetter, b_depth: usize) -> Ordering {
    (a.at, a_depth, &a.id).cmp(&(b.at, b_depth, &b.id))
}

/// The newest event of an item by [`newer`]: the one whose word the state is.
fn newest_event(events: &[EventLetter]) -> Option<&EventLetter> {
    let depths = event_depths(events);
    events
        .iter()
        .zip(depths)
        .enumerate()
        .max_by(|(_, (a, a_depth)), (_, (b, b_depth))| newer(a, *a_depth, b, *b_depth))
        .map(|(_, (event, _))| event)
}

/// The causal depth of every event among `events`: 0 for an event none of
/// whose `prev` names a held event, else one past the deepest held event it
/// names. A cycle (which no honest writer makes) is cut where it is met
/// again, so depth stays finite.
fn event_depths(events: &[EventLetter]) -> Vec<usize> {
    let mut held: HashMap<&str, usize> = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        held.entry(event.id.as_str()).or_insert(index);
    }
    let mut depths: Vec<Option<usize>> = vec![None; events.len()];
    let mut walking: HashSet<usize> = HashSet::new();
    (0..events.len())
        .map(|index| depth_of(index, events, &held, &mut depths, &mut walking))
        .collect()
}

/// The causal depth of the event at `index`, computing the held events it
/// names first. An event met again while it is being computed counts 0.
fn depth_of(
    index: usize,
    events: &[EventLetter],
    held: &HashMap<&str, usize>,
    depths: &mut [Option<usize>],
    walking: &mut HashSet<usize>,
) -> usize {
    if let Some(depth) = depths[index] {
        return depth;
    }
    if !walking.insert(index) {
        return 0;
    }
    let mut deepest: Option<usize> = None;
    for prev in &events[index].prev {
        let Some(&named) = held.get(prev.as_str()) else {
            continue;
        };
        let depth = depth_of(named, events, held, depths, walking);
        deepest = Some(deepest.map_or(depth, |seen| seen.max(depth)));
    }
    walking.remove(&index);
    let depth = deepest.map_or(0, |seen| seen + 1);
    depths[index] = Some(depth);
    depth
}

/// How far an event's instant may run past the clock and still count: ten
/// minutes of clock skew, no more.
const FUTURE_LIMIT_MICROS: i64 = 10 * 60 * 1_000_000;

/// Whether an event dated `at` counts at fold time. An instant more than
/// ten minutes past the receiver's clock is a claim about the future, not
/// an order: it is skipped the way an unauthorized event is, so no letter
/// can lock an item from the future.
#[must_use]
pub(super) fn dated_within_clock(at: i64, now_micros: i64) -> bool {
    at <= now_micros.saturating_add(FUTURE_LIMIT_MICROS)
}

/// One folder change the head asks for.
struct Planned {
    row: StoredMessageView,
    to_role: String,
}

impl Mailspace {
    /// Whether an event written from `event_domain` speaks for the state of
    /// an item: any prefix domain (`<8 hex>.local`) can, whoever authored
    /// the item. The server stamps every record with the sender's registered
    /// prefix and the pull refuses a prefix-form From it did not stamp, so
    /// such a From names a real member. A bare `local` or a legacy name
    /// domain is a claim anyone can make, so it never counts.
    fn event_speaks_for(event_domain: &str) -> bool {
        prefix_of_domain(event_domain).is_some()
    }

    fn item_chain(storage: &Storage, item: &HeldItem) -> Result<Chain, VivariumError> {
        let mut chain = Chain {
            events: Vec::new(),
            unauthorized: 0,
        };
        let now = chrono::Utc::now().timestamp_micros();
        for child in storage.reply_child_contents(&item.content_id)? {
            let Some(view) = storage.message_by_content_id(&child)? else {
                continue;
            };
            let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
                continue;
            };
            let Some(event) = parse_event(&bytes).filter(|event| event.of == item.rfc_id) else {
                continue;
            };
            if Self::event_speaks_for(&event.from_domain) && dated_within_clock(event.at, now) {
                chain.events.push(event);
            } else {
                chain.unauthorized += 1;
            }
        }
        Ok(chain)
    }

    /// The state the latest authorized event of an item says, if any.
    pub(super) fn item_head(
        storage: &Storage,
        item: &HeldItem,
    ) -> Result<Option<ItemEventKind>, VivariumError> {
        let chain = Self::item_chain(storage, item)?;
        Ok(newest_event(&chain.events).map(|head| head.kind))
    }

    /// The role that owns an item held here: the one that authored it, or the
    /// one that holds its sent copy when the author role is gone. `None` when
    /// another mailspace authored it.
    pub(super) fn item_author(&self, item: &HeldItem) -> Option<String> {
        let (local, domain) = item.from_addr.rsplit_once('@')?;
        if !self.is_own_domain(domain) {
            return None;
        }
        self.identity_owning_account(local).or_else(|| {
            item.rows
                .iter()
                .filter(|row| row.local_role == "sent")
                .find_map(|row| self.identity_owning_account(&row.account))
        })
    }

    /// Whether `identity` may close or reopen `item` for everyone: the role
    /// that authored it (or holds its sent copy), or a role of kind `mind`
    /// or `operator` — whichever mailspace authored the item. The read side
    /// ([`Self::event_speaks_for`]) believes any member's prefix domain, so
    /// no member owns an item's state.
    pub(super) fn may_close(&self, item: &HeldItem, identity: &str) -> bool {
        self.item_author(item).as_deref() == Some(identity)
            || self.identity_may_close_for_all(identity)
    }

    /// The rows the head `kind` moves: open work to `done` on a close, done
    /// rows back to the folder they closed from on a reopen. A row held for
    /// another mailspace's role moves like an own row: the state is the
    /// item's, not the holder's. A `sent` row follows only when it is the
    /// sender's only copy (every recipient is in another mailspace); with a
    /// local delivered copy the sent row is a record of the send, not the
    /// work.
    ///
    /// With `keep_reports`, a reopen leaves alone a `done` row held by a role
    /// that cannot accept the item: once the reopen has been applied, such a
    /// row is a later report, not state the event left behind.
    fn plan_item_moves(
        &self,
        storage: &Storage,
        item: &HeldItem,
        kind: ItemEventKind,
        keep_reports: bool,
    ) -> Result<Vec<Planned>, VivariumError> {
        let mut planned = Vec::new();
        for row in &item.rows {
            if row.absorbed_at.is_some() {
                continue;
            }
            let to_role = match (kind, row.local_role.as_str()) {
                (ItemEventKind::Done, "tasks" | "needs" | "wants") => "done".to_string(),
                (ItemEventKind::Done, "sent") if self.all_recipients_remote(row) => {
                    "done".to_string()
                }
                (ItemEventKind::Reopened, "done")
                    if keep_reports && self.is_report_row(item, row) =>
                {
                    continue;
                }
                (ItemEventKind::Reopened, "done") => self.reopen_role(storage, item, row)?,
                _ => continue,
            };
            planned.push(Planned {
                row: row.clone(),
                to_role,
            });
        }
        Ok(planned)
    }

    /// The folder a closed row returns to: where it was when it closed, else
    /// `sent` for the sender's only copy, else the folder of its kind.
    fn reopen_role(
        &self,
        storage: &Storage,
        item: &HeldItem,
        row: &StoredMessageView,
    ) -> Result<String, VivariumError> {
        let events = storage.list_mailspace_events(&row.message_id)?;
        let closed_from = events.iter().rev().find_map(|event| {
            let from = event.from_role.as_deref()?;
            (event.to_role.as_deref() == Some("done")
                && matches!(from, "tasks" | "needs" | "wants" | "sent"))
            .then(|| from.to_string())
        });
        if let Some(role) = closed_from {
            return Ok(role);
        }
        if self.all_recipients_remote(row) {
            return Ok("sent".into());
        }
        Ok(match item.kind {
            "need" => "needs",
            "want" => "wants",
            _ => "tasks",
        }
        .into())
    }

    /// Complete or reopen the backlog node of every row of `item`, with the
    /// siblings and join cascades the lifecycle already runs. Returns how
    /// many nodes changed.
    fn sync_item_nodes(
        &self,
        storage: &Storage,
        item: &HeldItem,
        kind: ItemEventKind,
        via: &str,
        joined: &mut Vec<String>,
    ) -> Result<usize, VivariumError> {
        let mut changed = 0;
        for row in &item.rows {
            if is_foreign_account(&row.account) {
                continue;
            }
            let handle = storage.display_handle(&row.message_id)?;
            let moved = match kind {
                ItemEventKind::Done => self.backlog_complete_item_collect(&handle, via, joined)?,
                ItemEventKind::Reopened => self.backlog_reopen_item_changed(&handle)?,
            };
            changed += usize::from(moved);
        }
        Ok(changed)
    }

    /// Settle one item against its newest event. Returns the rows moved plus
    /// the nodes changed, and counts ignored events into `tally`.
    fn settle_item(
        &self,
        storage: &mut Storage,
        content_id: &str,
        tally: &mut EventTally,
        keep_reports: bool,
    ) -> Result<(), VivariumError> {
        // Most work has no replies at all: skip the parse for it.
        if storage.reply_child_contents(content_id)?.is_empty() {
            return Ok(());
        }
        let Some(item) = held_item(storage, content_id)? else {
            return Ok(());
        };
        let chain = Self::item_chain(storage, &item)?;
        tally.ignored += chain.unauthorized;
        // A counted absorbed event seals the item with the state it names, so
        // the state events held beside it are only letters: none of them
        // moves a row, however new it is. A state event older than the seal
        // is ignored the same way — the absorb letter already named the
        // state.
        let sealed = property::counted_absorb(storage, &item)?.is_some();
        self.apply_property_events(storage, &item, tally)?;
        if sealed {
            return Ok(());
        }
        let Some(kind) = newest_event(&chain.events).map(|head| head.kind) else {
            return Ok(());
        };
        // Sealed work stays sealed only when every row of it is absorbed:
        // an item held only as foreign rows settles too, and its unabsorbed
        // rows move.
        if !item.rows.is_empty() && item.rows.iter().all(|row| row.absorbed_at.is_some()) {
            tally.ignored += 1;
            return Ok(());
        }
        let moves: Vec<RoleMove> = self
            .plan_item_moves(storage, &item, kind, keep_reports)?
            .iter()
            .map(|planned| role_move(&item, planned, kind, None))
            .collect();
        if !moves.is_empty() {
            storage.apply_item_event(&ItemEventWrite {
                reply_requests: &[],
                reply_data: None,
                parent_content_id: "",
                moves: &moves,
            })?;
        }
        let nodes = self.sync_item_nodes(storage, &item, kind, "via=event", &mut Vec::new())?;
        tally.applied += usize::from(!moves.is_empty() || nodes > 0);
        Ok(())
    }

    /// Settle the items the letters named by `content_ids` belong to: an event
    /// letter settles its item, a work item settles itself against the events
    /// held for it. Idempotent: an item already at its head changes nothing.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] on storage failure.
    pub fn apply_arrived_events(
        &self,
        content_ids: &[String],
    ) -> Result<EventTally, VivariumError> {
        let mut tally = EventTally::default();
        if content_ids.is_empty() {
            return Ok(tally);
        }
        let mut storage = self.storage()?;
        // An arriving event letter moves every row its head names; a work
        // item that arrives is only checked against the events it holds, and
        // a report made after a reopen stays.
        let mut by_event: BTreeSet<String> = BTreeSet::new();
        let mut by_item: BTreeSet<String> = BTreeSet::new();
        for content_id in content_ids {
            let Some(view) = storage.message_by_content_id(content_id)? else {
                continue;
            };
            let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
                continue;
            };
            match parse_event(&bytes) {
                Some(event) => {
                    if let Some(item) = storage.live_message_by_rfc_id(&event.of)? {
                        by_event.insert(item.content_id);
                    }
                }
                // A property event settles its item the same way, by the
                // item it names; when the item is not held yet, the letter
                // waits for it as a state event does.
                None => match parse_property_event(&bytes).map(|event| event.of) {
                    Some(of) => {
                        if let Some(item) = storage.live_message_by_rfc_id(&of)? {
                            by_event.insert(item.content_id);
                        }
                    }
                    None => {
                        by_item.insert(content_id.clone());
                    }
                },
            }
        }
        by_item.retain(|content_id| !by_event.contains(content_id));
        for content_id in &by_event {
            self.settle_item(&mut storage, content_id, &mut tally, false)?;
        }
        for content_id in &by_item {
            self.settle_item(&mut storage, content_id, &mut tally, true)?;
        }
        Ok(tally)
    }

    /// The current state of the item `handle` names, from its events: the
    /// newest one, and how many chain heads (a fork) are held. `None` when
    /// the item has no authorized event or is not work.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the handle does not resolve or on
    /// storage failure.
    pub fn item_event_state(&self, handle: &str) -> Result<Option<ItemEventState>, VivariumError> {
        let storage = self.storage()?;
        let message_id = storage.resolve_message_token(handle)?;
        let Some(view) = storage.message_by_id(&message_id)? else {
            return Ok(None);
        };
        let Some(item) = held_item(&storage, &view.content_id)? else {
            return Ok(None);
        };
        let chain = Self::item_chain(&storage, &item)?;
        let heads = chain_heads(&chain.events).len();
        Ok(newest_event(&chain.events).map(|newest| ItemEventState {
            state: newest.kind.header().to_string(),
            event: newest.id.clone(),
            heads,
        }))
    }

    /// What the item behind `content_id` would change to match its newest
    /// event: the event's state and the rows that disagree with it. `None`
    /// when nothing disagrees.
    pub(super) fn event_drift(
        &self,
        storage: &Storage,
        content_id: &str,
    ) -> Result<Option<(ItemEventKind, Vec<StoredMessageView>)>, VivariumError> {
        if storage.reply_child_contents(content_id)?.is_empty() {
            return Ok(None);
        }
        let Some(item) = held_item(storage, content_id)? else {
            return Ok(None);
        };
        let chain = Self::item_chain(storage, &item)?;
        let Some(kind) = newest_event(&chain.events).map(|head| head.kind) else {
            return Ok(None);
        };
        let rows: Vec<StoredMessageView> = self
            .plan_item_moves(storage, &item, kind, true)?
            .into_iter()
            .map(|planned| planned.row)
            .collect();
        Ok((!rows.is_empty()).then_some((kind, rows)))
    }

    /// Re-apply the newest event of the item behind `content_id` (the
    /// audit's repair).
    pub(super) fn repair_event_drift(&self, content_id: &str) -> Result<(), VivariumError> {
        let mut storage = self.storage()?;
        self.settle_item(&mut storage, content_id, &mut EventTally::default(), true)
    }
}

/// The lifecycle row for one folder move of an item-state event. `actor`
/// names the command, identity and note of the person who acted, for the row
/// they pointed at.
fn role_move(
    item: &HeldItem,
    planned: &Planned,
    kind: ItemEventKind,
    actor: Option<&emit::EmitRequest<'_>>,
) -> RoleMove {
    let row = &planned.row;
    let (command, identity, note) = match actor {
        Some(request) if request.before.message_id == row.message_id => (
            request.command.to_string(),
            Some(request.identity.to_string()),
            request.log_note(),
        ),
        _ => (kind.command(item.kind), None, Some("via=event".to_string())),
    };
    RoleMove {
        account: row.account.clone(),
        message_id: row.message_id.clone(),
        to_role: Some(planned.to_role.clone()),
        event: MailspaceEventInput {
            command,
            event_type: "moved".into(),
            actor_identity: identity.clone(),
            account: row.account.clone(),
            message_id: row.message_id.clone(),
            content_id: row.content_id.clone(),
            from_role: Some(row.local_role.clone()),
            to_role: Some(planned.to_role.clone()),
            from_identity: identity.clone(),
            to_identity: identity.or_else(|| Some(row.account.clone())),
            subject: row.subject.clone(),
            note,
        },
    }
}

#[cfg(test)]
#[path = "item_events_test.rs"]
mod tests;

#[cfg(test)]
#[path = "item_events_order_test.rs"]
mod order_tests;
