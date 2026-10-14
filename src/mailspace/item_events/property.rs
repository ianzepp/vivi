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
//! receiver never exports on its own. `marked`, `unmarked`, and `bound`
//! are parsed and authorized here and applied by later units; for them the
//! letter is held, and only counted.
//!
//! As with state events, only a letter from a prefix domain counts
//! (`event_speaks_for`): such a From names a real member. A bare `local`
//! or legacy name domain is a claim anyone can make: it is filed as plain
//! mail and counted ignored.

use mail_parser::{HeaderValue, MessageParser};

use super::{
    EVENT_AT_HEADER, EVENT_HEADER, EVENT_OF_HEADER, EventTally, HeldItem, ItemEventKind, Mailspace,
    date_micros, from_domain_of, rfc3339_micros, role_move,
};
use crate::error::VivariumError;
use crate::message::normalize_message_id;
use crate::storage::{ItemEventWrite, RoleMove, Storage};

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
    /// ignored, as for state events. The other kinds are parsed and
    /// authorized, and applied by later units.
    pub(super) fn apply_property_events(
        &self,
        storage: &mut Storage,
        item: &HeldItem,
        tally: &mut EventTally,
    ) -> Result<(), VivariumError> {
        let chain = property_chain(storage, item)?;
        tally.ignored += chain.unauthorized;
        let Some(seal) = earliest_absorb(&chain.events).cloned() else {
            return Ok(());
        };
        if item.rows.iter().all(|row| row.absorbed_at.is_some()) {
            tally.ignored += 1;
            return Ok(());
        }
        self.seal_item(storage, item, &seal, tally)
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
}
