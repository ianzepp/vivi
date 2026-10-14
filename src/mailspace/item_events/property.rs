//! Properties as letters: absorb, marks, and binds beside the state.
//!
//! A property event is a small reply letter on a need/task/want, shaped
//! like a state event: the same `X-Vivi-Event-Of` / `In-Reply-To` pairing
//! (both must name the item, else it is plain mail), the same optional
//! `X-Vivi-Event-At` (RFC 3339) — else the `Date` — as its instant, and
//! one payload header per kind:
//!
//! ```text
//! In-Reply-To: <item Message-ID>
//! X-Vivi-Event: absorbed | marked | unmarked | bound
//! X-Vivi-Event-Of: <item Message-ID>
//! X-Vivi-Event-At: <RFC 3339>                 (optional, all kinds)
//! X-Vivi-Event-Slug: <slug>                   (marked / unmarked)
//! X-Vivi-Event-Unit: <unit Message-ID>        (bound; one header per unit)
//! X-Vivi-Event-State: done | open             (absorbed)
//! ```
//!
//! `absorbed` says the writer sealed the item into its archive in the
//! named state — the state of every row of the item when the seal was
//! written. A member that holds the item brings its rows to that state and
//! seals them too; the writer's mailspace exports the archive copy, so a
//! receiver never exports on its own. `marked` and `unmarked` are applied
//! here: the letters are folded per slug and the newest one per slug moves
//! the unabsorbed rows' folded marks. `bound` is applied here too: the
//! units the letter names bind to the item's backlog node, as they bound
//! where the letter was written.
//!
//! As with state events, only a letter from a prefix domain counts
//! (`event_speaks_for`): such a From names a real member. A bare `local`
//! or legacy name domain is a claim anyone can make: it is filed as plain
//! mail and counted ignored.

use std::collections::BTreeMap;

use mail_parser::{HeaderValue, MessageParser};

use super::{
    EVENT_AT_HEADER, EVENT_HEADER, EVENT_OF_HEADER, EventTally, HeldItem, ItemEventKind, Mailspace,
    date_micros, from_domain_of, rfc3339_micros, role_move,
};
use crate::error::VivariumError;
use crate::mailspace::marks::{folded_marks, mark_event};
use crate::message::normalize_message_id;
use crate::storage::{
    BACKLOG_GRAPH_CODE, ItemEventWrite, MailspaceEvent, MailspaceEventInput, RoleMove, Storage,
    StoredMessageView, WorkGraphNodeRow,
};

const EVENT_UNIT_HEADER: &str = "X-Vivi-Event-Unit";
const EVENT_SLUG_HEADER: &str = "X-Vivi-Event-Slug";
const EVENT_STATE_HEADER: &str = "X-Vivi-Event-State";

/// What a property event puts on an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    /// The writer sealed the item into its archive in a named state.
    Absorbed,
    /// The item carries a mark (a named property with no state meaning).
    Marked,
    /// The item no longer carries a mark.
    Unmarked,
    /// Unit items are bound to the item (need lowering).
    Bound,
}

impl PropertyKind {
    /// The `X-Vivi-Event` value.
    #[must_use]
    pub fn header(self) -> &'static str {
        match self {
            Self::Absorbed => "absorbed",
            Self::Marked => "marked",
            Self::Unmarked => "unmarked",
            Self::Bound => "bound",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "absorbed" => Some(Self::Absorbed),
            "marked" => Some(Self::Marked),
            "unmarked" => Some(Self::Unmarked),
            "bound" => Some(Self::Bound),
            _ => None,
        }
    }
}

/// A property-event letter, read from its bytes. A letter counts only when
/// it names the item both as `Event-Of` and as the message it replies to,
/// and carries the payload its kind needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PropertyEvent {
    pub(crate) kind: PropertyKind,
    /// Normalized `Message-ID` of the event letter itself.
    pub(crate) id: String,
    /// Normalized `Message-ID` of the item.
    pub(crate) of: String,
    pub(crate) from_domain: String,
    /// When the event happened: `X-Vivi-Event-At` (RFC 3339) in Unix
    /// microseconds when present and parseable, else the `Date` header in
    /// whole seconds × 1 000 000 — the same instant a state event carries.
    pub(crate) at: i64,
    /// The `X-Vivi-Event-Slug` of a `marked` or `unmarked` event.
    pub(crate) slug: Option<String>,
    /// The normalized unit `Message-ID`s of a `bound` event.
    pub(crate) units: Vec<String>,
    /// The `X-Vivi-Event-State` of an `absorbed` event: `done` or `open`.
    pub(crate) state: Option<String>,
}

/// Read a property event from a letter's bytes; `None` for a letter that is
/// not one, or one without the payload its kind needs.
pub(crate) fn parse_property_event(bytes: &[u8]) -> Option<PropertyEvent> {
    let parsed = MessageParser::default().parse(bytes)?;
    let mut kind = None;
    let mut of = None;
    let mut at = None;
    let mut slug = None;
    let mut state = None;
    let mut units: Vec<String> = Vec::new();
    for header in parsed.headers() {
        let HeaderValue::Text(text) = header.value() else {
            continue;
        };
        let name = header.name();
        if name.eq_ignore_ascii_case(EVENT_HEADER) {
            kind = PropertyKind::parse(text);
        } else if name.eq_ignore_ascii_case(EVENT_OF_HEADER) {
            of = normalize_message_id(text);
        } else if name.eq_ignore_ascii_case(EVENT_AT_HEADER) {
            at = rfc3339_micros(text);
        } else if name.eq_ignore_ascii_case(EVENT_SLUG_HEADER) {
            slug = Some(text.trim().to_string());
        } else if name.eq_ignore_ascii_case(EVENT_STATE_HEADER) {
            state = Some(text.trim().to_ascii_lowercase());
        } else if name.eq_ignore_ascii_case(EVENT_UNIT_HEADER)
            && let Some(id) = normalize_message_id(text)
            && !units.contains(&id)
        {
            units.push(id);
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
    let kind = kind?;
    let carries_payload = match kind {
        PropertyKind::Marked | PropertyKind::Unmarked => slug.is_some(),
        PropertyKind::Bound => !units.is_empty(),
        PropertyKind::Absorbed => matches!(state.as_deref(), Some("done" | "open")),
    };
    carries_payload.then_some(PropertyEvent {
        kind,
        id: parsed.message_id().and_then(normalize_message_id)?,
        of,
        from_domain: from_domain_of(&parsed)?,
        at: at.unwrap_or_else(|| date_micros(&parsed)),
        slug,
        units,
        state,
    })
}

/// The property events held for one item, and how many named it from a
/// domain that speaks for no member.
struct PropertyChain {
    events: Vec<PropertyEvent>,
    unauthorized: usize,
}

/// Every property event held for `item` that a member wrote, plus the count
/// of the others. State events and property events are read apart: a state
/// event letter never parses as a property event, and the reverse too.
fn property_chain(storage: &Storage, item: &HeldItem) -> Result<PropertyChain, VivariumError> {
    let mut chain = PropertyChain {
        events: Vec::new(),
        unauthorized: 0,
    };
    for child in storage.reply_child_contents(&item.content_id)? {
        let Some(view) = storage.message_by_content_id(&child)? else {
            continue;
        };
        let Ok(bytes) = storage.read_listed_blob(&view.blob_relpath) else {
            continue;
        };
        let Some(event) = parse_property_event(&bytes).filter(|event| event.of == item.rfc_id)
        else {
            continue;
        };
        if Mailspace::event_speaks_for(&event.from_domain) {
            chain.events.push(event);
        } else {
            chain.unauthorized += 1;
        }
    }
    Ok(chain)
}

/// The earliest counted `absorbed` event among `events` — smallest `at`,
/// then smallest `Message-ID` — the one whose seal this mailspace follows.
fn earliest_absorb(events: &[PropertyEvent]) -> Option<&PropertyEvent> {
    events
        .iter()
        .filter(|event| event.kind == PropertyKind::Absorbed)
        .min_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)))
}

/// The earliest counted `absorbed` event held for `item`, if any.
pub(super) fn counted_absorb(
    storage: &Storage,
    item: &HeldItem,
) -> Result<Option<PropertyEvent>, VivariumError> {
    let chain = property_chain(storage, item)?;
    Ok(earliest_absorb(&chain.events).cloned())
}

impl Mailspace {
    /// Settle the property events held for `item`. A counted `absorbed`
    /// event seals the item: its rows are brought to the state the seal
    /// names — every state event held is only a letter next to the seal,
    /// however new — and each unabsorbed row is then sealed, without
    /// exporting to the archive (the writer's mailspace exports). When the
    /// item is already sealed, every later event of any kind is counted
    /// ignored, as for state events. With no seal, the counted
    /// `marked`/`unmarked` letters are folded per slug and the newest one
    /// per slug moves the unabsorbed rows' folded marks, and the counted
    /// `bound` events are applied: their units bind to the item's backlog
    /// node.
    pub(super) fn apply_property_events(
        &self,
        storage: &mut Storage,
        item: &HeldItem,
        tally: &mut EventTally,
    ) -> Result<(), VivariumError> {
        let chain = property_chain(storage, item)?;
        tally.ignored += chain.unauthorized;
        let Some(seal) = earliest_absorb(&chain.events).cloned() else {
            Self::apply_mark_letters(storage, item, &chain.events, tally)?;
            return self.apply_bound_events(storage, item, &chain.events, tally);
        };
        if item.rows.iter().all(|row| row.absorbed_at.is_some()) {
            tally.ignored += 1;
            return Ok(());
        }
        self.seal_item(storage, item, &seal, tally)
    }

    /// Apply the counted `bound` events held for `item` (a need): every unit
    /// the letter names that this mailspace holds as work with a backlog
    /// node binds to the item's node, as it bound where the letter was
    /// written — unless the unit's controlling bind, the newest `bound`
    /// letter that moved it anywhere, is newer than this letter. The bind's
    /// `unit_bound` event carries the letter's own instant and
    /// `Message-ID`, so the floor a letter must beat is the same on every
    /// member that holds the same letters, never a receiver clock. The join
    /// rule then runs, so a need whose units are all done completes here as
    /// it would where the bind was made. A unit not held yet, and a need
    /// with no node here (held foreign), are skipped.
    fn apply_bound_events(
        &self,
        storage: &mut Storage,
        item: &HeldItem,
        events: &[PropertyEvent],
        tally: &mut EventTally,
    ) -> Result<(), VivariumError> {
        let mut binds: Vec<&PropertyEvent> = events
            .iter()
            .filter(|event| event.kind == PropertyKind::Bound)
            .collect();
        if binds.is_empty() {
            return Ok(());
        }
        binds.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
        let Some(backlog) = storage.work_graph_by_code(BACKLOG_GRAPH_CODE)? else {
            return Ok(());
        };
        let Some(need) = item_node(storage, item)? else {
            return Ok(());
        };
        let mut bound = false;
        for event in binds {
            let handles = units_to_bind(storage, &backlog.handle, event)?;
            if handles.is_empty() {
                continue;
            }
            storage.bind_backlog_units(
                &backlog.handle,
                &need.source_id,
                &handles,
                event.at,
                &event.id,
            )?;
            bound = true;
        }
        tally.applied += usize::from(bound);
        if bound {
            let need_source = need.source_id.clone();
            self.backlog_complete_if_join_complete(&need_source, &mut Vec::new())?;
        }
        Ok(())
    }

    /// Bring `item` to the state `seal` names and seal every unabsorbed row
    /// of it, as the writer's absorb letter says it sealed the item.
    fn seal_item(
        &self,
        storage: &mut Storage,
        item: &HeldItem,
        seal: &PropertyEvent,
        tally: &mut EventTally,
    ) -> Result<(), VivariumError> {
        let kind = match seal.state.as_deref() {
            Some("done") => ItemEventKind::Done,
            _ => ItemEventKind::Reopened,
        };
        let moves: Vec<RoleMove> = self
            .plan_item_moves(storage, item, kind, true)?
            .iter()
            .map(|planned| role_move(item, planned, kind, None))
            .collect();
        if !moves.is_empty() {
            storage.apply_item_event(&ItemEventWrite {
                reply_requests: &[],
                reply_data: None,
                parent_content_id: "",
                moves: &moves,
            })?;
        }
        self.sync_item_nodes(storage, item, kind, "via=absorb", &mut Vec::new())?;
        for row in &item.rows {
            if row.absorbed_at.is_none() {
                storage.absorb_message(&row.account, &row.message_id, &row.account)?;
            }
        }
        tally.applied += 1;
        Ok(())
    }

    /// Bring the unabsorbed rows of `item` — own or foreign-held — to what
    /// the newest `marked`/`unmarked` letter per slug says, appending one
    /// local mark event per row so `folded_marks` yields the letter's state.
    /// A letter older than a row's newest local mark event for its slug
    /// changes nothing: a later local change by a person is not undone by
    /// an older letter that arrives late. Idempotent: a state that already
    /// matches appends nothing.
    fn apply_mark_letters(
        storage: &mut Storage,
        item: &HeldItem,
        events: &[PropertyEvent],
        tally: &mut EventTally,
    ) -> Result<(), VivariumError> {
        let mut inputs: Vec<MailspaceEventInput> = Vec::new();
        for letter in newest_mark_letters(events) {
            let slug = letter.slug.as_deref().unwrap_or_default();
            let marked = letter.kind == PropertyKind::Marked;
            for row in &item.rows {
                if row.absorbed_at.is_some() {
                    continue;
                }
                inputs.extend(row_mark_input(storage, row, item, slug, marked, letter.at)?);
            }
        }
        storage.append_mailspace_events(&inputs)?;
        tally.applied += usize::from(!inputs.is_empty());
        Ok(())
    }
}

/// The newest `marked`/`unmarked` letter per slug by `(at, Message-ID)`,
/// sorted by slug: the one whose word the row's folded marks follow.
fn newest_mark_letters(events: &[PropertyEvent]) -> Vec<&PropertyEvent> {
    let mut newest: BTreeMap<&str, &PropertyEvent> = BTreeMap::new();
    for event in events {
        if !matches!(event.kind, PropertyKind::Marked | PropertyKind::Unmarked) {
            continue;
        }
        let Some(slug) = event.slug.as_deref() else {
            continue;
        };
        let newer = newest
            .get(slug)
            .is_none_or(|held| (event.at, &event.id) > (held.at, &held.id));
        if newer {
            newest.insert(slug, event);
        }
    }
    newest.into_values().collect()
}

/// The local mark event `row` needs to match `marked` for `slug`, or `None`
/// when the row already folds to that state or the letter is older than the
/// row's newest local mark event for the slug.
fn row_mark_input(
    storage: &Storage,
    row: &StoredMessageView,
    item: &HeldItem,
    slug: &str,
    marked: bool,
    letter_at: i64,
) -> Result<Option<MailspaceEventInput>, VivariumError> {
    let events = storage.list_mailspace_events(&row.message_id)?;
    let carries = folded_marks(&events).iter().any(|held| held == slug);
    if carries == marked {
        return Ok(None);
    }
    let local = events
        .iter()
        .filter(|event| mark_event_of(event, slug))
        .filter_map(|event| rfc3339_micros(&event.occurred_at))
        .max();
    if local.is_some_and(|at| letter_at <= at) {
        return Ok(None);
    }
    Ok(Some(mark_event(row, None, item.kind, marked, slug)))
}

/// Whether `event` is a mark event for `slug`.
fn mark_event_of(event: &MailspaceEvent, slug: &str) -> bool {
    matches!(event.event_type.as_str(), "marked" | "unmarked")
        && event.note.as_deref() == Some(slug)
}

/// The item's own backlog node, if this mailspace tracks it; a held
/// foreign item with no node here is skipped.
fn item_node(
    storage: &Storage,
    item: &HeldItem,
) -> Result<Option<WorkGraphNodeRow>, VivariumError> {
    for row in &item.rows {
        let handle = storage.display_handle(&row.message_id)?;
        if let Some(node) = Mailspace::backlog_node_of(storage, &handle)? {
            return Ok(Some(node));
        }
    }
    Ok(None)
}

/// The node handles of the units `event` binds here: held as work with a
/// backlog node, and named by a letter newer than the unit's controlling
/// bind — the newest `bound` letter that moved it, wherever that bind was
/// made — so an older letter arriving late moves nothing and a newer one
/// re-targets.
fn units_to_bind(
    storage: &Storage,
    graph_handle: &str,
    event: &PropertyEvent,
) -> Result<Vec<String>, VivariumError> {
    let mut handles = Vec::new();
    for unit_id in &event.units {
        let Some(row) = storage.live_message_by_rfc_id(unit_id)? else {
            continue;
        };
        let Some(node) = Mailspace::backlog_node_of(storage, &row.handle)? else {
            continue;
        };
        let floor = newest_unit_bound(storage, graph_handle, &node.handle)?;
        if floor.is_none_or(|(at, id)| (event.at, event.id.as_str()) > (at, id.as_str())) {
            handles.push(node.handle.clone());
        }
    }
    Ok(handles)
}

/// The unit's controlling bind — the `(instant, Message-ID)` of its newest
/// `unit_bound` graph event, the floor a letter must be newer than to move
/// the unit. Every stamp names the `bound` letter that made the bind (an
/// empty id for a bind that wrote no letter), so the floor is a function
/// of the letters held, the same on every member that pulled them.
fn newest_unit_bound(
    storage: &Storage,
    graph_handle: &str,
    node_handle: &str,
) -> Result<Option<(i64, String)>, VivariumError> {
    let mut newest: Option<(i64, String)> = None;
    for event in storage.list_work_graph_events_after(0)? {
        if event.graph_handle != graph_handle
            || event.node_handle.as_deref() != Some(node_handle)
            || event.event_type != "unit_bound"
        {
            continue;
        }
        let Ok(at) = chrono::DateTime::parse_from_rfc3339(&event.occurred_at) else {
            continue;
        };
        let stamp = (at.timestamp_micros(), letter_of(&event.note));
        if newest.as_ref().is_none_or(|held| &stamp > held) {
            newest = Some(stamp);
        }
    }
    Ok(newest)
}

/// The `Message-ID` a `unit_bound` event's note names, or an empty string
/// when the bind wrote no letter.
fn letter_of(note: &Option<String>) -> String {
    note.as_deref()
        .and_then(|note| note.split(' ').find(|part| part.starts_with("letter=<")))
        .and_then(|part| {
            part.strip_prefix("letter=<")
                .and_then(|id| id.strip_suffix('>'))
        })
        .map(str::to_string)
        .unwrap_or_default()
}
