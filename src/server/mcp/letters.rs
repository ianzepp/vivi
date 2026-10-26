//! Compose and parse letters for the MCP surface, reusing vivi's
//! `build_compose_draft` so outbound bytes match the CLI. The server mints
//! the traveling id (decided item 15). Event headers follow vivi's
//! `X-Vivi-Event` / `Of` / `At` / `Prev` shape so a checkout that pulls the
//! letter folds the same state, and [`fold_item_states`] folds them by the
//! same rule: "When two machines disagree about a need state, by definition,
//! the last update or event created against that need is controlling." The
//! order is the client's: the event's instant, then its causal depth through
//! `X-Vivi-Event-Prev`, then its `Message-ID`; an event more than ten
//! minutes ahead of the clock at fold time is not counted.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::letter::plain::{CLOSE_EVENTS, CloseFront, EmptyFront, render as render_front};
use crate::letter::{FORMAT_HEADER, FORMAT_VERSION, Gated, gate, of_message};
use crate::message::{ComposeDraft, build_compose_draft, normalize_message_id};

use crate::server::auth::AuthError;
use crate::server::identities::IdentityRecord;
use crate::server::projects::{
    AppendItem, AppendOutcome, ProjectError, ProjectRecord, ProjectStore,
};

const HANDLE_LEN: usize = 8;
/// The role every mailspace has that may close or reopen any item.
const OPERATOR_ROLE: &str = "operator";

#[derive(Debug, Clone, Default)]
pub struct ParsedLetter {
    pub from: String,
    pub from_domain: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub rfc_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub kind_header: Option<String>,
    pub event: Option<String>,
    pub event_of: Option<String>,
    pub event_prev: Vec<String>,
    /// `X-Vivi-Event-At` as Unix microseconds, when present and RFC 3339.
    pub event_at: Option<i64>,
    /// The `Date` header as Unix microseconds (whole seconds), when parseable.
    pub date: Option<i64>,
    /// `X-Vivi-Event-Slug` of a `marked` / `unmarked` event.
    pub event_slug: Option<String>,
    /// `X-Vivi-Event-Unit` ids of a `bound` event, normalized.
    pub event_units: Vec<String>,
    /// `X-Vivi-Event-State` of an `absorbed` event, lowercased.
    pub event_state: Option<String>,
    pub body: String,
}

/// What the newest counted events say about one item.
#[derive(Debug, Clone)]
pub struct ItemState {
    /// `done` or `open`.
    pub state: String,
    /// The `Message-ID` of the event that decided `state`: the newest
    /// `done`/`reopened`, or the `absorbed` event that sealed the item.
    pub event: Option<String>,
    /// The state events no later event names as `Prev`, oldest first by the
    /// order key: what a new event names as its own `X-Vivi-Event-Prev`.
    pub heads: Vec<String>,
    /// The newest counted `done`/`reopened` event's instant in Unix
    /// microseconds, 0 when there is none: a new event is never stamped at
    /// or below it, as the client stamps one.
    pub newest_at: i64,
    /// A counted `absorbed` event sealed the item; it is final.
    pub absorbed: bool,
    /// Slugs whose newest `marked`/`unmarked` event is `marked`, sorted.
    pub marks: Vec<String>,
    /// Units whose newest `bound` event names this item, sorted.
    pub units: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ComposedLetter {
    pub traveling_id: String,
    pub raw_eml: Vec<u8>,
    pub to: String,
    pub kind: Option<String>,
}

pub fn caller_address(identity: &IdentityRecord) -> Result<String, String> {
    let prefix = identity.mailspace_prefix().ok_or_else(|| {
        "this identity has no mailspace prefix; it cannot send from MCP".to_owned()
    })?;
    Ok(format!("{}@{prefix}.local", identity.name))
}

/// The address an MCP state change is written From: the conventional
/// `operator` role of the caller's mailspace. The member that owns the prefix
/// counts an own-domain event only when its local part is the item's author
/// or a role of kind `mind` or `operator` there, and a registration name is
/// neither, so an event From `name@<prefix>.local` would be ignored by the
/// very member that holds the item while the server and every other member
/// counted it. The state change is the member's, taken in its operator role.
pub fn operator_address(identity: &IdentityRecord) -> Result<String, String> {
    let prefix = identity.mailspace_prefix().ok_or_else(|| {
        "this identity has no mailspace prefix; it cannot send from MCP".to_owned()
    })?;
    Ok(format!("{OPERATOR_ROLE}@{prefix}.local"))
}

pub fn mint_traveling_id() -> Result<String, AuthError> {
    let mut raw = [0u8; 12];
    getrandom::fill(&mut raw).map_err(|_| AuthError::Entropy)?;
    Ok(format!("msg_{}", hex_encode(&raw)))
}

/// The headers of a letter and its body.
pub fn parse_letter(raw: &[u8]) -> ParsedLetter {
    let text = String::from_utf8_lossy(raw);
    let (header_text, body) = split_headers(&text);
    let mut parsed = parse_header_block(header_text);
    parsed.body = body.to_owned();
    parsed
}

/// The headers of a letter alone, `body` left empty: what a fold, a status,
/// or a listing reads, without a copy of every body it passes over.
pub fn parse_headers(raw: &[u8]) -> ParsedLetter {
    let text = String::from_utf8_lossy(raw);
    let (header_text, _) = split_headers(&text);
    parse_header_block(header_text)
}

fn parse_header_block(header_text: &str) -> ParsedLetter {
    let mut parsed = ParsedLetter::default();
    for (name, value) in folded_headers(header_text) {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "from" => {
                parsed.from = value.clone();
                parsed.from_domain = address_domain(&value);
            }
            "to" => parsed.to = split_addresses(&value),
            "cc" => parsed.cc = split_addresses(&value),
            "subject" => parsed.subject = value,
            "message-id" => parsed.rfc_id = normalize_message_id(&value),
            "in-reply-to" => parsed.in_reply_to = normalize_message_id(&value),
            "references" => {
                parsed.references = value
                    .split_whitespace()
                    .filter_map(normalize_message_id)
                    .collect();
            }
            "x-vivi-kind" => parsed.kind_header = Some(value.to_ascii_lowercase()),
            "x-vivi-event" => parsed.event = Some(value.to_ascii_lowercase()),
            "x-vivi-event-of" => parsed.event_of = normalize_message_id(&value),
            "x-vivi-event-prev" => {
                if let Some(id) = normalize_message_id(&value) {
                    parsed.event_prev.push(id);
                }
            }
            "x-vivi-event-at" => parsed.event_at = rfc3339_micros(&value),
            "x-vivi-event-slug" => parsed.event_slug = Some(value),
            "x-vivi-event-state" => parsed.event_state = Some(value.to_ascii_lowercase()),
            "x-vivi-event-unit" => {
                if let Some(id) = normalize_message_id(&value)
                    && !parsed.event_units.contains(&id)
                {
                    parsed.event_units.push(id);
                }
            }
            "date" => parsed.date = date_micros(&value),
            _ => {}
        }
    }
    parsed
}

/// The compose gate on a body about to be stored: vivi's storage-free
/// `letter::gate`, whose error names what to fix. A body the gate refuses
/// is refused before anything is written.
fn gate_body(kind: &str, body: &str) -> Result<Gated, String> {
    gate(kind, body).map_err(|e| e.to_string())
}

/// The format header the gate called for: a formatted body carries
/// `X-Vivi-Format: 1`; a kind the gate passes through unchanged stores none.
fn set_format_header(eml: String, gated: &Gated) -> Result<String, String> {
    if gated.formatted {
        set_header(&eml, FORMAT_HEADER, FORMAT_VERSION)
    } else {
        Ok(eml)
    }
}

pub fn compose_send(
    identity: &IdentityRecord,
    to: &[String],
    subject: &str,
    body: &str,
    kind: &str,
) -> Result<ComposedLetter, String> {
    let gated = gate_body(kind, body)?;
    let from = caller_address(identity)?;
    if to.is_empty() {
        return Err("send needs at least one to address".to_owned());
    }
    let traveling_id = mint_traveling_id().map_err(|_| "entropy unavailable".to_owned())?;
    let prefix = identity.mailspace_prefix().unwrap_or("server");
    let mut eml = build_compose_draft(&ComposeDraft {
        from,
        to: to.to_vec(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.to_owned(),
        body: body.to_owned(),
        html_body: None,
    })
    .map_err(|e| e.to_string())?;
    eml = set_header(
        &eml,
        "Message-ID",
        &format!("<{traveling_id}@{prefix}.local>"),
    )?;
    eml = set_header(&eml, "X-Vivi-Kind", kind)?;
    eml = set_format_header(eml, &gated)?;
    let envelope_kind = match kind {
        "need" | "task" | "want" | "chat" => Some(kind.to_owned()),
        _ => None,
    };
    Ok(ComposedLetter {
        traveling_id,
        raw_eml: eml.into_bytes(),
        to: to.join(","),
        kind: envelope_kind,
    })
}

pub fn compose_reply(
    identity: &IdentityRecord,
    parent: &ProjectRecord,
    body: &str,
) -> Result<ComposedLetter, String> {
    let from = caller_address(identity)?;
    let (header_kind, envelope_kind) = if parent.kind.as_deref() == Some("chat") {
        ("chat", Some("chat".to_owned()))
    } else {
        ("mail", None)
    };
    let gated = gate_body(header_kind, body)?;
    let parsed = parse_letter(&parent.raw_eml);
    let mut to = Vec::new();
    push_unique(&mut to, parent_from_address(&parsed, parent));
    for address in parsed.to.iter().chain(parsed.cc.iter()) {
        if !address.eq_ignore_ascii_case(&from) {
            push_unique(&mut to, address.clone());
        }
    }
    if to.is_empty() {
        to.push(from.clone());
    }
    let subject = reply_subject(&parsed.subject);
    let traveling_id = mint_traveling_id().map_err(|_| "entropy unavailable".to_owned())?;
    let prefix = identity.mailspace_prefix().unwrap_or("server");
    let mut eml = build_compose_draft(&ComposeDraft {
        from,
        to: to.clone(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject,
        body: body.to_owned(),
        html_body: None,
    })
    .map_err(|e| e.to_string())?;
    eml = set_header(
        &eml,
        "Message-ID",
        &format!("<{traveling_id}@{prefix}.local>"),
    )?;
    eml = set_header(&eml, "X-Vivi-Kind", header_kind)?;
    eml = set_format_header(eml, &gated)?;
    if let Some(rfc_id) = parsed
        .rfc_id
        .as_deref()
        .or(Some(parent.message_id.as_str()))
    {
        eml = set_header(&eml, "In-Reply-To", &format!("<{rfc_id}>"))?;
        let mut refs = parsed.references.clone();
        if !refs.iter().any(|r| r == rfc_id) {
            refs.push(rfc_id.to_owned());
        }
        if !refs.is_empty() {
            let value = refs
                .iter()
                .map(|id| format!("<{id}>"))
                .collect::<Vec<_>>()
                .join(" ");
            eml = set_header(&eml, "References", &value)?;
        }
    }
    Ok(ComposedLetter {
        traveling_id,
        raw_eml: eml.into_bytes(),
        to: to.join(","),
        kind: envelope_kind,
    })
}

/// An item-state event on `parent` (a need, task, or want), composed the way
/// the client composes one: written From the caller's `operator` role
/// ([`operator_address`]), addressed to the item's From first, then its To
/// and Cc minus the sender; `X-Vivi-Kind: mail` and envelope kind `mail`, as
/// the client sends every event letter, so an old client files it as mail
/// and never as work (the server serves it to every member by its
/// `X-Vivi-Event` header, not its kind); and stamped with
/// `X-Vivi-Event-At` (`at`, RFC 3339) and one `X-Vivi-Event-Prev` per head
/// the item's chain has (`prevs`).
///
/// The letter is format 1: the close metadata (`verdict`, `branch`,
/// `commit`, `repos`) of a `done` or `reopened` event rides the body's
/// front matter as a `CloseFront` (an empty front stores the note as
/// written, with no fence), and the composed bytes are parsed back with
/// `of_message`, so a writer bug is an error at write time.
pub fn compose_event(
    identity: &IdentityRecord,
    parent: &ProjectRecord,
    note: &str,
    event: &str,
    at: &str,
    prevs: &[String],
    close: CloseFront,
) -> Result<ComposedLetter, String> {
    if !matches!(parent.kind.as_deref(), Some("need" | "task" | "want")) {
        return Err("an event letter names a need, task, or want".to_owned());
    }
    // Close metadata travels as front matter; an empty front stores the
    // note as written, with no fence (vivi's own event composer's rule).
    let body = if CLOSE_EVENTS.contains(&event) {
        render_front(&close, note)
    } else {
        render_front(&EmptyFront {}, note)
    }
    .map_err(|error| format!("the {event} letter: {error}"))?;
    let from = operator_address(identity)?;
    let parsed = parse_letter(&parent.raw_eml);
    let mut seen: Vec<String> = Vec::new();
    let mut keep = |addresses: Vec<String>| -> Vec<String> {
        let mut kept = Vec::new();
        for address in addresses {
            let is_sender = address.eq_ignore_ascii_case(&from);
            if !is_sender
                && !seen
                    .iter()
                    .any(|other| other.eq_ignore_ascii_case(&address))
            {
                seen.push(address.clone());
                kept.push(address);
            }
        }
        kept
    };
    let mut to = keep(vec![parent_from_address(&parsed, parent)]);
    to.extend(keep(parsed.to.clone()));
    let cc = keep(parsed.cc.clone());
    if to.is_empty() && cc.is_empty() {
        // Nobody else is on the item: the letter is the sender's own record.
        to.push(from.clone());
    }
    let envelope = to
        .iter()
        .chain(cc.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    let traveling_id = mint_traveling_id().map_err(|_| "entropy unavailable".to_owned())?;
    let prefix = identity.mailspace_prefix().unwrap_or("server");
    let mut eml = build_compose_draft(&ComposeDraft {
        from,
        to,
        cc,
        bcc: Vec::new(),
        subject: reply_subject(&parsed.subject),
        body,
        html_body: None,
    })
    .map_err(|e| e.to_string())?;
    eml = set_header(
        &eml,
        "Message-ID",
        &format!("<{traveling_id}@{prefix}.local>"),
    )?;
    eml = set_header(&eml, "X-Vivi-Kind", "mail")?;
    eml = set_header(&eml, FORMAT_HEADER, FORMAT_VERSION)?;
    let of = parsed
        .rfc_id
        .clone()
        .unwrap_or_else(|| parent.message_id.clone());
    eml = set_header(&eml, "In-Reply-To", &format!("<{of}>"))?;
    let mut refs = parsed.references.clone();
    if !refs.iter().any(|r| r == &of) {
        refs.push(of.clone());
    }
    let refs = refs
        .iter()
        .map(|id| format!("<{id}>"))
        .collect::<Vec<_>>()
        .join(" ");
    eml = set_header(&eml, "References", &refs)?;
    eml = set_header(&eml, "X-Vivi-Event", event)?;
    eml = set_header(&eml, "X-Vivi-Event-Of", &format!("<{of}>"))?;
    eml = set_header(&eml, "X-Vivi-Event-At", at)?;
    for prev in prevs {
        eml = add_header(&eml, "X-Vivi-Event-Prev", &format!("<{prev}>"))?;
    }
    // A writer bug is an error at write time, never a bad letter on disk:
    // the composed bytes must parse, and `of_message` refuses a format-1
    // letter that carries a header W3-B4 moved into front matter.
    of_message(eml.as_bytes(), "").map_err(|error| format!("the {event} letter: {error}"))?;
    Ok(ComposedLetter {
        traveling_id,
        raw_eml: eml.into_bytes(),
        to: envelope,
        kind: None,
    })
}

pub fn append_composed(
    store: &ProjectStore,
    project_id: &str,
    identity: &IdentityRecord,
    composed: ComposedLetter,
) -> Result<AppendOutcome, ProjectError> {
    let item = AppendItem::new(
        &composed.traveling_id,
        &identity.name,
        Some(&composed.to),
        composed.kind.as_deref(),
        composed.raw_eml,
    )?
    .with_from_prefix(identity.mailspace_prefix());
    store.append(project_id, &item)
}

/// How far an event's instant may run past the server clock at fold time
/// and still count, in microseconds: ten minutes of clock skew, no more —
/// the client's bound, applied the same way. An event dated further ahead is
/// not counted at all, as the client does not count it, so no letter can
/// lock an item from the future. The test is made against the clock at the
/// moment of the fold and never against the letter's append time, so the
/// same letters fold the same on the server and on every checkout.
pub const EVENT_FUTURE_LIMIT_MICROS: i64 = 10 * 60 * 1_000_000;

/// What an item event says. `done`, `reopened` and `absorbed` decide state;
/// `marked`/`unmarked` and `bound` ride beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    Done,
    Reopened,
    Absorbed,
    Marked,
    Unmarked,
    Bound,
}

impl EventKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "done" => Some(Self::Done),
            "reopened" => Some(Self::Reopened),
            "absorbed" => Some(Self::Absorbed),
            "marked" => Some(Self::Marked),
            "unmarked" => Some(Self::Unmarked),
            "bound" => Some(Self::Bound),
            _ => None,
        }
    }
}

/// A counted event letter: from a member, naming its item, with the payload
/// its kind needs.
struct CountedEvent {
    /// Normalized `Message-ID` of the event letter.
    id: String,
    /// Normalized `Message-ID` of the item.
    of: String,
    kind: EventKind,
    /// The instant it counts at, in Unix microseconds: `X-Vivi-Event-At`,
    /// else the `Date` in whole seconds.
    at: i64,
    prev: Vec<String>,
    slug: Option<String>,
    units: Vec<String>,
    /// The state an `absorbed` event sealed: `done` or `open`.
    state: Option<String>,
}

/// Whether `record`'s sender speaks for its `From` domain: the domain is the
/// `<prefix>.local` the server stamped for the sender at append. The client's
/// pull refuses a prefix-form `From` that is not the sender's own, so such a
/// `From` names a real member; a bare `local`, a legacy name domain, or a
/// prefix the sender does not hold is a claim anyone can make and never
/// counts.
fn from_member(record: &ProjectRecord, letter: &ParsedLetter) -> bool {
    let (Some(domain), Some(prefix)) =
        (letter.from_domain.as_deref(), record.from_prefix.as_deref())
    else {
        return false;
    };
    domain.eq_ignore_ascii_case(&format!("{prefix}.local"))
}

/// Whether an event dated `at` counts at fold time `now` (both Unix
/// microseconds): an instant more than [`EVENT_FUTURE_LIMIT_MICROS`] past the
/// clock is a claim about the future, not an order.
fn dated_within_clock(at: i64, now: i64) -> bool {
    at <= now.saturating_add(EVENT_FUTURE_LIMIT_MICROS)
}

/// The event `record` is, when it counts at fold time `now`: a member's
/// letter that names its item both as `X-Vivi-Event-Of` and as the message
/// it replies to, of a kind this server folds, with the payload that kind
/// needs, dated no further than [`EVENT_FUTURE_LIMIT_MICROS`] ahead of
/// `now`. Anything else is plain mail to the fold (an unknown kind is
/// skipped, not an error).
fn counted_event(record: &ProjectRecord, letter: &ParsedLetter, now: i64) -> Option<CountedEvent> {
    let kind = EventKind::parse(letter.event.as_deref()?)?;
    let of = letter.event_of.clone()?;
    if letter.in_reply_to.as_deref() != Some(of.as_str()) {
        return None;
    }
    let id = letter.rfc_id.clone()?;
    if !from_member(record, letter) {
        return None;
    }
    let carries_payload = match kind {
        EventKind::Marked | EventKind::Unmarked => letter.event_slug.is_some(),
        EventKind::Bound => !letter.event_units.is_empty(),
        EventKind::Absorbed => matches!(letter.event_state.as_deref(), Some("done" | "open")),
        EventKind::Done | EventKind::Reopened => true,
    };
    if !carries_payload {
        return None;
    }
    let at = letter.event_at.or(letter.date).unwrap_or(0);
    if !dated_within_clock(at, now) {
        return None;
    }
    Some(CountedEvent {
        id,
        of,
        kind,
        at,
        prev: letter.event_prev.clone(),
        slug: letter.event_slug.clone(),
        units: letter.event_units.clone(),
        state: letter.event_state.clone(),
    })
}

/// The order key of an event, as the client keys it: the instant, then the
/// causal depth (see [`event_depths`]), then the `Message-ID`, compared
/// lexicographically. A total order, so every ordering of the same letters
/// picks the same newest event. The clock beats the chain: a later instant
/// stays newer even over an event that names it as `Prev`.
fn order_key(event: &CountedEvent, depth: usize) -> (i64, usize, &str) {
    (event.at, depth, event.id.as_str())
}

/// The causal depth of every state event among `events`: 0 for an event
/// none of whose `Prev` names a held event, else one past the deepest held
/// event it names. A `Prev` that names no counted state event (a property
/// event, an event not held, one dated past the clock) adds nothing. A
/// cycle, which no honest writer makes, is cut where it is met again, so
/// depth stays finite. Walks with an explicit stack: a chain a member makes
/// as long as it likes must not be able to overflow the server's.
fn event_depths(events: &[&CountedEvent]) -> Vec<usize> {
    let mut held: HashMap<&str, usize> = HashMap::new();
    for (index, event) in events.iter().enumerate() {
        held.entry(event.id.as_str()).or_insert(index);
    }
    let mut depths: Vec<Option<usize>> = vec![None; events.len()];
    let mut walking = vec![false; events.len()];
    for start in 0..events.len() {
        if depths[start].is_some() {
            continue;
        }
        // One frame per event being computed: its index, the position of
        // the next `Prev` to look at, and the deepest held event seen.
        let mut stack: Vec<(usize, usize, Option<usize>)> = vec![(start, 0, None)];
        walking[start] = true;
        while let Some(frame) = stack.last_mut() {
            let (index, next, deepest) = *frame;
            let Some(prev) = events[index].prev.get(next) else {
                walking[index] = false;
                let depth = deepest.map_or(0, |seen| seen + 1);
                depths[index] = Some(depth);
                stack.pop();
                if let Some(parent) = stack.last_mut() {
                    parent.2 = Some(parent.2.map_or(depth, |seen| seen.max(depth)));
                }
                continue;
            };
            frame.1 += 1;
            let Some(&named) = held.get(prev.as_str()) else {
                continue;
            };
            match depths[named] {
                Some(depth) => frame.2 = Some(deepest.map_or(depth, |seen| seen.max(depth))),
                // Met again while it is being computed: counts 0.
                None if walking[named] => frame.2 = Some(deepest.unwrap_or(0)),
                None => {
                    walking[named] = true;
                    stack.push((named, 0, None));
                }
            }
        }
    }
    depths.into_iter().map(|depth| depth.unwrap_or(0)).collect()
}

/// The state events no other names as `Prev`, oldest first by the order key:
/// the events a new event names as its own `X-Vivi-Event-Prev`. A cycle
/// (which no honest writer makes) has no head, so every event is one then.
fn chain_heads(events: &[&CountedEvent], depths: &[usize]) -> Vec<String> {
    let named: HashSet<&str> = events
        .iter()
        .flat_map(|event| event.prev.iter().map(String::as_str))
        .collect();
    let mut heads: Vec<usize> = (0..events.len())
        .filter(|&index| !named.contains(events[index].id.as_str()))
        .collect();
    if heads.is_empty() {
        heads = (0..events.len()).collect();
    }
    heads.sort_by_key(|&index| order_key(events[index], depths[index]));
    heads
        .into_iter()
        .map(|index| events[index].id.clone())
        .collect()
}

/// The seal of an item: its earliest `absorbed` event by `(at, Message-ID)`,
/// the one whose word every member follows.
fn earliest_absorb(events: &[CountedEvent]) -> Option<&CountedEvent> {
    events
        .iter()
        .filter(|event| event.kind == EventKind::Absorbed)
        .min_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)))
}

/// Whether `event` still moves marks and binds: an item takes nothing after
/// its seal.
fn before_seal(event: &CountedEvent, seal: Option<&CountedEvent>) -> bool {
    seal.is_none_or(|seal| (event.at, &event.id) <= (seal.at, &seal.id))
}

/// Fold the event letters onto their items, keyed by the item's normalized
/// `Message-ID`, by the client's rule: the last event created against an item
/// controls. Reads the server clock for the future bound; see
/// [`fold_item_states_at`].
pub fn fold_item_states(records: &[ProjectRecord]) -> HashMap<String, ItemState> {
    fold_item_states_at(records, chrono::Utc::now().timestamp_micros())
}

/// [`fold_item_states`] at the clock `now` (Unix microseconds), the same
/// function of the letters held that the client folds:
///
/// - Any member's event counts, whoever authored the item ([`from_member`]).
/// - An event whose instant is more than [`EVENT_FUTURE_LIMIT_MICROS`] past
///   `now` is not counted at all.
/// - The newest `done`/`reopened` wins by its order key: the instant
///   (`X-Vivi-Event-At`, else `Date`; never `appended_at`), then the causal
///   depth through `X-Vivi-Event-Prev`, then the larger `Message-ID`
///   ([`order_key`]).
/// - `absorbed` is final: the earliest seal names the state, and no later
///   event of any kind moves anything.
/// - Marks: the newest `marked`/`unmarked` per slug, by `(instant,
///   Message-ID)`. Bind: the newest `bound` per unit, which belongs to the
///   item that letter names. A property event carries no `Prev` that the
///   client reads, so its depth is always 0 and the key is the same one.
pub fn fold_item_states_at(records: &[ProjectRecord], now: i64) -> HashMap<String, ItemState> {
    let mut fold = EventFold::new(now);
    for record in records {
        fold.observe(record);
    }
    fold.finish()
}

/// The event letters of a project gathered one record at a time, so a read
/// that pages through every letter folds as it goes and keeps only the
/// counted events, never the letters: [`fold_item_states_at`] over a slice
/// and a scan that [`observe`](Self::observe)s each letter fold the same.
pub struct EventFold {
    now: i64,
    by_item: HashMap<String, Vec<CountedEvent>>,
}

impl EventFold {
    /// A fold at the clock `now` (Unix microseconds), the instant the future
    /// bound is read against for every letter it is shown.
    pub fn new(now: i64) -> Self {
        Self {
            now,
            by_item: HashMap::new(),
        }
    }

    /// Take `record` in when it counts as an event. Returns whether the
    /// letter carries `X-Vivi-Event` at all, counted or not: such a letter
    /// is never an item.
    pub fn observe(&mut self, record: &ProjectRecord) -> bool {
        let letter = parse_headers(&record.raw_eml);
        if letter.event.is_none() {
            return false;
        }
        if let Some(event) = counted_event(record, &letter, self.now) {
            self.by_item
                .entry(event.of.clone())
                .or_default()
                .push(event);
        }
        true
    }

    /// Fold the events held onto their items, by [`fold_item_states_at`]'s
    /// rule.
    pub fn finish(self) -> HashMap<String, ItemState> {
        fold_events(self.by_item)
    }
}

fn fold_events(by_item: HashMap<String, Vec<CountedEvent>>) -> HashMap<String, ItemState> {
    // A unit belongs to the item named by its newest bind, across items.
    let mut newest_bind: HashMap<&str, &CountedEvent> = HashMap::new();
    for events in by_item.values() {
        let seal = earliest_absorb(events);
        for event in events {
            if event.kind != EventKind::Bound || !before_seal(event, seal) {
                continue;
            }
            for unit in &event.units {
                let wins = newest_bind
                    .get(unit.as_str())
                    .is_none_or(|held| (event.at, &event.id) > (held.at, &held.id));
                if wins {
                    newest_bind.insert(unit, event);
                }
            }
        }
    }
    let mut units_of: HashMap<&str, Vec<String>> = HashMap::new();
    for (unit, event) in &newest_bind {
        units_of
            .entry(event.of.as_str())
            .or_default()
            .push((*unit).to_owned());
    }
    let mut out = HashMap::new();
    for (of, events) in &by_item {
        let seal = earliest_absorb(events);
        let state_events: Vec<&CountedEvent> = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Done | EventKind::Reopened))
            .collect();
        let depths = event_depths(&state_events);
        let head = state_events
            .iter()
            .zip(&depths)
            .max_by_key(|(event, depth)| order_key(event, **depth))
            .map(|(event, _)| *event);
        let (state, event) = match (seal, head) {
            (Some(seal), _) => (seal.state.as_deref() == Some("done"), Some(seal.id.clone())),
            (None, Some(head)) => (head.kind == EventKind::Done, Some(head.id.clone())),
            (None, None) => (false, None),
        };
        let mut marks: BTreeMap<&str, &CountedEvent> = BTreeMap::new();
        for event in events {
            let (EventKind::Marked | EventKind::Unmarked) = event.kind else {
                continue;
            };
            let Some(slug) = event.slug.as_deref() else {
                continue;
            };
            if !before_seal(event, seal) {
                continue;
            }
            let wins = marks
                .get(slug)
                .is_none_or(|held| (event.at, &event.id) > (held.at, &held.id));
            if wins {
                marks.insert(slug, event);
            }
        }
        let mut units = units_of.remove(of.as_str()).unwrap_or_default();
        units.sort();
        out.insert(
            of.clone(),
            ItemState {
                state: if state { "done" } else { "open" }.to_owned(),
                event,
                heads: chain_heads(&state_events, &depths),
                newest_at: state_events.iter().map(|event| event.at).max().unwrap_or(0),
                absorbed: seal.is_some(),
                marks: marks
                    .into_iter()
                    .filter(|(_, event)| event.kind == EventKind::Marked)
                    .map(|(slug, _)| slug.to_owned())
                    .collect(),
                units,
            },
        );
    }
    out
}

/// The folded state of the item a record is, by its `Message-ID` or its
/// traveling id.
pub fn state_of<'a>(
    record: &ProjectRecord,
    letter: &ParsedLetter,
    states: &'a HashMap<String, ItemState>,
) -> Option<&'a ItemState> {
    letter
        .rfc_id
        .as_deref()
        .and_then(|rfc_id| states.get(rfc_id))
        .or_else(|| states.get(&record.message_id))
}

/// `event` for an event letter (never an item), `mail` for plain mail, else
/// the folded `open` or `done` of the work item.
pub fn item_status(
    record: &ProjectRecord,
    letter: &ParsedLetter,
    states: &HashMap<String, ItemState>,
) -> String {
    if letter.event.is_some() {
        return "event".to_owned();
    }
    if record.kind.is_none() {
        return "mail".to_owned();
    }
    state_of(record, letter, states).map_or_else(|| "open".to_owned(), |state| state.state.clone())
}

pub fn handle_of(message_id: &str) -> Option<&str> {
    message_id
        .get(4..4 + HANDLE_LEN)
        .filter(|h| h.len() == HANDLE_LEN && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn valid_handle(handle: &str) -> bool {
    handle.len() == HANDLE_LEN && handle.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parent_from_address(parsed: &ParsedLetter, parent: &ProjectRecord) -> String {
    if let Some(address) = email_address(&parsed.from) {
        return address;
    }
    if let Some(prefix) = &parent.from_prefix {
        return format!("{}@{prefix}.local", parent.from);
    }
    parent.from.clone()
}

/// An RFC 3339 instant as Unix microseconds.
mod headers;
use headers::{
    add_header, address_domain, date_micros, email_address, folded_headers, hex_encode,
    push_unique, reply_subject, rfc3339_micros, set_header, split_addresses, split_headers,
};

#[cfg(test)]
#[path = "letters_test.rs"]
mod tests;
