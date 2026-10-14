//! Compose and parse letters for the MCP surface, reusing vivi's
//! `build_compose_draft` so outbound bytes match the CLI. The server mints
//! the traveling id (decided item 15). Event headers follow vivi's
//! `X-Vivi-Event` / `Of` / `At` / `Prev` shape so a checkout that pulls the
//! letter folds the same state, and [`fold_item_states`] folds them by the
//! same rule: "When two machines disagree about a need state, by definition,
//! the last update or event created against that need is controlling."

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use vivi::message::{ComposeDraft, build_compose_draft, normalize_message_id};

use crate::auth::AuthError;
use crate::identities::IdentityRecord;
use crate::projects::{AppendItem, AppendOutcome, ProjectError, ProjectRecord, ProjectStore};

const HANDLE_LEN: usize = 8;

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
    pub depends_on: Vec<String>,
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
    /// The state events no later event names as `Prev`, oldest first: what a
    /// new event names as its own `X-Vivi-Event-Prev`.
    pub heads: Vec<String>,
    /// The newest counted event's instant in Unix microseconds (after the
    /// append-time bound): a new event is never stamped below it.
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

pub fn mint_traveling_id() -> Result<String, AuthError> {
    let mut raw = [0u8; 12];
    getrandom::fill(&mut raw).map_err(|_| AuthError::Entropy)?;
    Ok(format!("msg_{}", hex_encode(&raw)))
}

pub fn parse_letter(raw: &[u8]) -> ParsedLetter {
    let text = String::from_utf8_lossy(raw);
    let (header_text, body) = split_headers(&text);
    let mut parsed = ParsedLetter {
        body: body.to_owned(),
        ..ParsedLetter::default()
    };
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
            "x-vivi-depends-on" => parsed.depends_on.push(value),
            _ => {}
        }
    }
    parsed
}

pub fn compose_send(
    identity: &IdentityRecord,
    to: &[String],
    subject: &str,
    body: &str,
    kind: &str,
    depends_on: &[String],
) -> Result<ComposedLetter, String> {
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
    for dep in depends_on {
        eml = add_header(&eml, "X-Vivi-Depends-On", dep)?;
    }
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
    let (header_kind, envelope_kind) = if parent.kind.as_deref() == Some("chat") {
        ("chat", Some("chat".to_owned()))
    } else {
        ("mail", None)
    };
    eml = set_header(&eml, "X-Vivi-Kind", header_kind)?;
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
/// the client composes one: addressed to the item's From first, then its To
/// and Cc minus the sender; carrying the item's own envelope kind so every
/// member receives it (the header kind is `mail`, as the client writes it);
/// and stamped with `X-Vivi-Event-At` (`at`, RFC 3339) and one
/// `X-Vivi-Event-Prev` per head the item's chain has (`prevs`).
pub fn compose_event(
    identity: &IdentityRecord,
    parent: &ProjectRecord,
    note: &str,
    event: &str,
    at: &str,
    prevs: &[String],
) -> Result<ComposedLetter, String> {
    let kind = match parent.kind.as_deref() {
        Some(kind @ ("need" | "task" | "want")) => kind.to_owned(),
        _ => return Err("an event letter names a need, task, or want".to_owned()),
    };
    let from = caller_address(identity)?;
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
        body: note.to_owned(),
        html_body: None,
    })
    .map_err(|e| e.to_string())?;
    eml = set_header(
        &eml,
        "Message-ID",
        &format!("<{traveling_id}@{prefix}.local>"),
    )?;
    eml = set_header(&eml, "X-Vivi-Kind", "mail")?;
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
    Ok(ComposedLetter {
        traveling_id,
        raw_eml: eml.into_bytes(),
        to: envelope,
        kind: Some(kind),
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

/// How far past a letter's server append time its own event instant may claim
/// to be before the server stops believing it, in microseconds.
///
/// Chosen: five minutes, applied to the event's resolved instant — its
/// `X-Vivi-Event-At`, or its `Date` when it carries none. An instant later
/// than `appended_at + 5 minutes` is treated as the append time itself, so a
/// far-future stamp cannot outrank every later event and lock the item; an
/// earlier or merely ahead-by-a-little stamp is kept as written. Five minutes
/// is room for a member's clock running ahead of the server's, not room for a
/// forged year. The bound moves the instant to the append time rather than
/// discarding the letter: the event still counts, as the newest thing that
/// verifiably happened when it arrived. A client that folds without this
/// bound can disagree with the server about a letter the bound moved.
pub const EVENT_AT_SKEW_MICROS: i64 = 5 * 60 * 1_000_000;

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
    /// The instant it counts at, in Unix microseconds, after the append-time
    /// bound ([`EVENT_AT_SKEW_MICROS`]).
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

/// An instant bounded by the letter's append time; see
/// [`EVENT_AT_SKEW_MICROS`].
fn bounded_at(at: i64, appended_at: &str) -> i64 {
    match rfc3339_micros(appended_at) {
        Some(appended) if at > appended.saturating_add(EVENT_AT_SKEW_MICROS) => appended,
        _ => at,
    }
}

/// The event `record` is, when it counts: a member's letter that names its
/// item both as `X-Vivi-Event-Of` and as the message it replies to, of a
/// kind this server folds, with the payload that kind needs. Anything else
/// is plain mail to the fold (an unknown kind is skipped, not an error).
fn counted_event(record: &ProjectRecord, letter: &ParsedLetter) -> Option<CountedEvent> {
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
    Some(CountedEvent {
        id,
        of,
        kind,
        at: bounded_at(at, &record.appended_at),
        prev: letter.event_prev.clone(),
        slug: letter.event_slug.clone(),
        units: letter.event_units.clone(),
        state: letter.event_state.clone(),
    })
}

/// How `a` stands against `b` in age, as the client orders events: the
/// larger instant wins; on equal instants a direct successor (it names the
/// other as `Prev`) wins; then the larger `Message-ID`. The clock beats the
/// chain, so a later instant stays newer even over an event that names it.
fn newer(a: &CountedEvent, b: &CountedEvent) -> Ordering {
    let successor = |x: &CountedEvent, y: &CountedEvent| x.prev.iter().any(|prev| prev == &y.id);
    a.at.cmp(&b.at)
        .then_with(|| match (successor(a, b), successor(b, a)) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => Ordering::Equal,
        })
        .then_with(|| a.id.cmp(&b.id))
}

/// The state events no other names as `Prev`, oldest first. A cycle (which
/// no honest writer makes) has no head, so every event is one then.
fn chain_heads(events: &[&CountedEvent]) -> Vec<String> {
    let named: HashSet<&str> = events
        .iter()
        .flat_map(|event| event.prev.iter().map(String::as_str))
        .collect();
    let mut heads: Vec<&CountedEvent> = events
        .iter()
        .copied()
        .filter(|event| !named.contains(event.id.as_str()))
        .collect();
    if heads.is_empty() {
        heads = events.to_vec();
    }
    heads.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
    heads.into_iter().map(|head| head.id.clone()).collect()
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
/// controls.
///
/// - Any member's event counts, whoever authored the item ([`from_member`]).
/// - The newest `done`/`reopened` wins by its `X-Vivi-Event-At`, else its
///   `Date`; never by `appended_at`. Ties go to the direct successor, then to
///   the larger `Message-ID` ([`newer`]).
/// - An instant more than [`EVENT_AT_SKEW_MICROS`] after the letter's append
///   time counts as the append time.
/// - `absorbed` is final: the earliest seal names the state, and no later
///   event of any kind moves anything.
/// - Marks: the newest `marked`/`unmarked` per slug. Bind: the newest
///   `bound` per unit, which belongs to the item that letter names.
pub fn fold_item_states(records: &[ProjectRecord]) -> HashMap<String, ItemState> {
    let mut by_item: HashMap<String, Vec<CountedEvent>> = HashMap::new();
    for record in records {
        let letter = parse_letter(&record.raw_eml);
        if let Some(event) = counted_event(record, &letter) {
            by_item.entry(event.of.clone()).or_default().push(event);
        }
    }
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
        let head = state_events.iter().copied().max_by(|a, b| newer(a, b));
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
                heads: chain_heads(&state_events),
                newest_at: events.iter().map(|event| event.at).max().unwrap_or(0),
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
fn rfc3339_micros(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|time| time.timestamp_micros())
}

/// A `Date` header as Unix microseconds, in whole seconds.
fn date_micros(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(value.trim())
        .ok()
        .map(|time| time.timestamp().saturating_mul(1_000_000))
}

fn email_address(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if let Some(start) = trimmed.find('<')
        && let Some(end) = trimmed[start + 1..].find('>')
    {
        return Some(trimmed[start + 1..start + 1 + end].to_owned());
    }
    trimmed.contains('@').then(|| trimmed.to_owned())
}

fn address_domain(raw: &str) -> Option<String> {
    let address = email_address(raw)?;
    address
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim().to_ascii_lowercase())
}

fn split_addresses(raw: &str) -> Vec<String> {
    raw.split(',')
        .filter_map(|part| email_address(part))
        .collect()
}

fn split_headers(text: &str) -> (&str, &str) {
    if let Some(pos) = text.find("\r\n\r\n") {
        return (&text[..pos], &text[pos + 4..]);
    }
    if let Some(pos) = text.find("\n\n") {
        return (&text[..pos], &text[pos + 2..]);
    }
    (text, "")
}

fn folded_headers(header_text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name = String::new();
    let mut value = String::new();
    for line in header_text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        if !name.is_empty() {
            out.push((std::mem::take(&mut name), value.trim().to_owned()));
        }
        match line.split_once(':') {
            Some((n, v)) => {
                name = n.trim().to_owned();
                value = v.trim().to_owned();
            }
            None => {
                name.clear();
                value.clear();
            }
        }
    }
    if !name.is_empty() {
        out.push((name, value.trim().to_owned()));
    }
    out
}

fn add_header(eml: &str, name: &str, value: &str) -> Result<String, String> {
    let newline = if eml.contains("\r\n") { "\r\n" } else { "\n" };
    let separator = format!("{newline}{newline}");
    let (headers, body) = eml
        .split_once(&separator)
        .ok_or_else(|| "composed letter has no header/body separator".to_owned())?;
    Ok(format!(
        "{headers}{newline}{name}: {value}{separator}{body}"
    ))
}

fn set_header(eml: &str, name: &str, value: &str) -> Result<String, String> {
    let newline = if eml.contains("\r\n") { "\r\n" } else { "\n" };
    let separator = format!("{newline}{newline}");
    let (headers, body) = eml
        .split_once(&separator)
        .ok_or_else(|| "composed letter has no header/body separator".to_owned())?;
    let needle = format!("{name}:");
    let mut replaced = false;
    let mut out = String::new();
    for line in headers.lines() {
        if !replaced
            && line.len() >= needle.len()
            && line[..needle.len()].eq_ignore_ascii_case(&needle)
        {
            if !out.is_empty() {
                out.push_str(newline);
            }
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            replaced = true;
        } else {
            if !out.is_empty() {
                out.push_str(newline);
            }
            out.push_str(line);
        }
    }
    if !replaced {
        out.push_str(newline);
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
    }
    Ok(format!("{out}{separator}{body}"))
}

fn reply_subject(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        "Re: (no subject)".to_owned()
    } else if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("re:") {
        trimmed.to_owned()
    } else {
        format!("Re: {trimmed}")
    }
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !value.is_empty() && !list.iter().any(|seen| seen.eq_ignore_ascii_case(&value)) {
        list.push(value);
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vivi::storage::is_traveling_id;

    #[test]
    fn traveling_ids_match_vivi_shape() {
        let id = mint_traveling_id().unwrap();
        assert!(is_traveling_id(&id), "{id}");
        assert_eq!(handle_of(&id).unwrap().len(), 8);
    }

    #[test]
    fn fold_prefers_the_unreferenced_head() {
        let item = ProjectRecord {
            sequence: 1,
            message_id: "msg_aaaaaaaa0000000000000001".into(),
            from: "alice".into(),
            from_prefix: Some("aaaaaaaa".into()),
            to: Some("hand@bbbbbbbb.local".into()),
            kind: Some("need".into()),
            content_sha256: "x".into(),
            appended_at: "2026-10-12T00:00:00Z".into(),
            raw_eml: b"From: alice@aaaaaaaa.local\nTo: hand@bbbbbbbb.local\n\
                 Message-ID: <need-1@aaaaaaaa.local>\nSubject: s\n\nbody\n"
                .to_vec(),
        };
        let done = ProjectRecord {
            sequence: 2,
            message_id: "msg_aaaaaaaa0000000000000002".into(),
            from: "alice".into(),
            from_prefix: Some("aaaaaaaa".into()),
            to: Some("hand@bbbbbbbb.local".into()),
            kind: None,
            content_sha256: "y".into(),
            appended_at: "2026-10-12T00:00:01Z".into(),
            raw_eml: b"From: alice@aaaaaaaa.local\nTo: hand@bbbbbbbb.local\n\
                 Message-ID: <done-1@aaaaaaaa.local>\nIn-Reply-To: <need-1@aaaaaaaa.local>\n\
                 X-Vivi-Event: done\nX-Vivi-Event-Of: <need-1@aaaaaaaa.local>\n\ndone\n"
                .to_vec(),
        };
        let states = fold_item_states(&[item, done]);
        assert_eq!(states["need-1@aaaaaaaa.local"].state, "done");
    }

    const AT: &str = "2026-10-12T12:00:00Z";

    fn record(
        sequence: u64,
        rfc_id: &str,
        sender: &str,
        kind: Option<&str>,
        appended_at: &str,
        headers: &str,
    ) -> ProjectRecord {
        let prefix = sender.split_once('@').unwrap().1.split_once('.').unwrap().0;
        ProjectRecord {
            sequence,
            message_id: format!("msg_{sequence:024x}"),
            from: sender.split_once('@').unwrap().0.to_owned(),
            from_prefix: Some(prefix.to_owned()),
            to: Some("hand@bbbbbbbb.local".into()),
            kind: kind.map(str::to_owned),
            content_sha256: format!("{sequence:064x}"),
            appended_at: appended_at.to_owned(),
            raw_eml: format!(
                "From: {sender}\nTo: hand@bbbbbbbb.local\nMessage-ID: <{rfc_id}>\n\
                 Subject: s\n{headers}\nbody\n"
            )
            .into_bytes(),
        }
    }

    fn need(sequence: u64, rfc_id: &str) -> ProjectRecord {
        record(
            sequence,
            rfc_id,
            "alice@aaaaaaaa.local",
            Some("need"),
            AT,
            "",
        )
    }

    /// An event letter on `of`, written by `sender`, travelling under the
    /// item's kind like the client's.
    fn event(
        sequence: u64,
        rfc_id: &str,
        of: &str,
        sender: &str,
        kind: &str,
        appended_at: &str,
        extra: &str,
    ) -> ProjectRecord {
        record(
            sequence,
            rfc_id,
            sender,
            Some("need"),
            appended_at,
            &format!(
                "In-Reply-To: <{of}>\nX-Vivi-Event: {kind}\nX-Vivi-Event-Of: <{of}>\n{extra}\n"
            ),
        )
    }

    const BOB: &str = "bob@bbbbbbbb.local";

    fn state_of_item(records: &[ProjectRecord], item: &str) -> Option<String> {
        fold_item_states(records)
            .get(item)
            .map(|state| state.state.clone())
    }

    #[test]
    fn a_members_event_closes_another_members_item() {
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "ev-1@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "done",
                AT,
                "",
            ),
        ];
        assert_eq!(
            state_of_item(&records, "need-1@aaaaaaaa.local").as_deref(),
            Some("done")
        );
    }

    #[test]
    fn an_event_from_a_domain_the_sender_does_not_hold_does_not_count() {
        let item = need(1, "need-1@aaaaaaaa.local");
        // bob is stamped bbbbbbbb but writes as aaaaaaaa: a forged member.
        let mut forged = event(
            2,
            "ev-1@aaaaaaaa.local",
            "need-1@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "done",
            AT,
            "",
        );
        forged.from_prefix = Some("bbbbbbbb".into());
        // A legacy name domain is a claim anyone can make.
        let mut legacy = event(
            3,
            "ev-2@bob.local",
            "need-1@aaaaaaaa.local",
            "bob@bbbbbbbb.local",
            "done",
            AT,
            "",
        );
        legacy.raw_eml = String::from_utf8(legacy.raw_eml)
            .unwrap()
            .replace("bob@bbbbbbbb.local", "bob@bob.local")
            .into_bytes();
        // No prefix stamped at all (a legacy identity).
        let mut unstamped = event(
            4,
            "ev-3@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            AT,
            "",
        );
        unstamped.from_prefix = None;
        let records = [item, forged, legacy, unstamped];
        assert!(!fold_item_states(&records).contains_key("need-1@aaaaaaaa.local"));
    }

    #[test]
    fn an_event_must_reply_to_the_item_it_names() {
        let mut stray = event(
            2,
            "ev-1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            AT,
            "",
        );
        stray.raw_eml = String::from_utf8(stray.raw_eml)
            .unwrap()
            .replace("In-Reply-To: <need-1@aaaaaaaa.local>\n", "")
            .into_bytes();
        assert!(
            !fold_item_states(&[need(1, "need-1@aaaaaaaa.local"), stray])
                .contains_key("need-1@aaaaaaaa.local")
        );
    }

    #[test]
    fn the_newer_event_at_wins_whatever_the_append_order() {
        // The reopen was appended first but stamped later: it wins.
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "ev-1@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "reopened",
                "2026-10-12T12:00:10Z",
                "X-Vivi-Event-At: 2026-10-12T12:00:09.500000Z\n",
            ),
            event(
                3,
                "ev-2@aaaaaaaa.local",
                "need-1@aaaaaaaa.local",
                "alice@aaaaaaaa.local",
                "done",
                "2026-10-12T12:00:20Z",
                "X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\n",
            ),
        ];
        let states = fold_item_states(&records);
        let state = &states["need-1@aaaaaaaa.local"];
        assert_eq!(state.state, "open");
        assert_eq!(state.event.as_deref(), Some("ev-1@bbbbbbbb.local"));
    }

    #[test]
    fn the_date_header_is_the_instant_when_event_at_is_absent() {
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "ev-1@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "done",
                "2026-10-12T12:00:00Z",
                "Date: Mon, 12 Oct 2026 12:00:30 +0000\n",
            ),
            event(
                3,
                "ev-2@aaaaaaaa.local",
                "need-1@aaaaaaaa.local",
                "alice@aaaaaaaa.local",
                "reopened",
                "2026-10-12T12:00:40Z",
                "Date: Mon, 12 Oct 2026 12:00:20 +0000\n",
            ),
        ];
        assert_eq!(
            state_of_item(&records, "need-1@aaaaaaaa.local").as_deref(),
            Some("done")
        );
    }

    #[test]
    fn equal_instants_go_to_the_successor_then_the_larger_message_id() {
        let stamp = "X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\n";
        // zzz-done is the larger id, but reopen names it as Prev: reopen wins.
        let successor = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "zzz-done@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "done",
                AT,
                stamp,
            ),
            event(
                3,
                "aaa-reopen@aaaaaaaa.local",
                "need-1@aaaaaaaa.local",
                "alice@aaaaaaaa.local",
                "reopened",
                AT,
                &format!("{stamp}X-Vivi-Event-Prev: <zzz-done@bbbbbbbb.local>\n"),
            ),
        ];
        assert_eq!(
            state_of_item(&successor, "need-1@aaaaaaaa.local").as_deref(),
            Some("open")
        );
        // No Prev either way: the larger Message-ID wins.
        let by_id = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "zzz-done@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "done",
                AT,
                stamp,
            ),
            event(
                3,
                "aaa-reopen@aaaaaaaa.local",
                "need-1@aaaaaaaa.local",
                "alice@aaaaaaaa.local",
                "reopened",
                AT,
                stamp,
            ),
        ];
        assert_eq!(
            state_of_item(&by_id, "need-1@aaaaaaaa.local").as_deref(),
            Some("done")
        );
    }

    #[test]
    fn a_far_future_at_counts_as_the_append_time() {
        // done is stamped in 2099 but appended at 12:00:00; the reopen at
        // 12:03:00 is genuinely later and must win.
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "ev-1@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "done",
                "2026-10-12T12:00:00Z",
                "X-Vivi-Event-At: 2099-01-01T00:00:00.000000Z\n",
            ),
            event(
                3,
                "ev-2@aaaaaaaa.local",
                "need-1@aaaaaaaa.local",
                "alice@aaaaaaaa.local",
                "reopened",
                "2026-10-12T12:03:00Z",
                "X-Vivi-Event-At: 2026-10-12T12:03:00.000000Z\n",
            ),
        ];
        let states = fold_item_states(&records);
        assert_eq!(states["need-1@aaaaaaaa.local"].state, "open");
        // On its own the 2099 stamp counts as its append time, which is what
        // a new event must be stamped above.
        let states = fold_item_states(&records[..2]);
        let appended = chrono::DateTime::parse_from_rfc3339("2026-10-12T12:00:00Z")
            .unwrap()
            .timestamp_micros();
        assert_eq!(states["need-1@aaaaaaaa.local"].state, "done");
        assert_eq!(states["need-1@aaaaaaaa.local"].newest_at, appended);
    }

    #[test]
    fn an_at_within_five_minutes_of_the_append_time_is_kept() {
        let near = |at: &str| {
            [
                need(1, "need-1@aaaaaaaa.local"),
                event(
                    2,
                    "ev-1@bbbbbbbb.local",
                    "need-1@aaaaaaaa.local",
                    BOB,
                    "done",
                    "2026-10-12T12:00:00Z",
                    &format!("X-Vivi-Event-At: {at}\n"),
                ),
                event(
                    3,
                    "ev-2@aaaaaaaa.local",
                    "need-1@aaaaaaaa.local",
                    "alice@aaaaaaaa.local",
                    "reopened",
                    "2026-10-12T12:03:30Z",
                    "X-Vivi-Event-At: 2026-10-12T12:03:00.000000Z\n",
                ),
            ]
        };
        // Four minutes ahead: believed, so the later-stamped done wins.
        assert_eq!(
            state_of_item(
                &near("2026-10-12T12:04:00.000000Z"),
                "need-1@aaaaaaaa.local"
            )
            .as_deref(),
            Some("done")
        );
        // Six minutes ahead: bounded to 12:00:00, so the reopen wins.
        assert_eq!(
            state_of_item(
                &near("2026-10-12T12:06:00.000000Z"),
                "need-1@aaaaaaaa.local"
            )
            .as_deref(),
            Some("open")
        );
    }

    #[test]
    fn absorbed_is_final_for_later_events() {
        let sealed_done = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "seal@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "absorbed",
                AT,
                "X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\nX-Vivi-Event-State: done\n",
            ),
            event(
                3,
                "late@aaaaaaaa.local",
                "need-1@aaaaaaaa.local",
                "alice@aaaaaaaa.local",
                "reopened",
                "2026-10-12T12:01:00Z",
                "X-Vivi-Event-At: 2026-10-12T12:01:00.000000Z\n",
            ),
        ];
        let states = fold_item_states(&sealed_done);
        let state = &states["need-1@aaaaaaaa.local"];
        assert_eq!(state.state, "done");
        assert!(state.absorbed);
        assert_eq!(state.event.as_deref(), Some("seal@bbbbbbbb.local"));

        // Sealed open: a done, even a newer one, changes nothing.
        let sealed_open = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "seal@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "absorbed",
                AT,
                "X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\nX-Vivi-Event-State: open\n",
            ),
            event(
                3,
                "late@aaaaaaaa.local",
                "need-1@aaaaaaaa.local",
                "alice@aaaaaaaa.local",
                "done",
                "2026-10-12T12:01:00Z",
                "X-Vivi-Event-At: 2026-10-12T12:01:00.000000Z\n",
            ),
        ];
        assert_eq!(
            state_of_item(&sealed_open, "need-1@aaaaaaaa.local").as_deref(),
            Some("open")
        );
    }

    #[test]
    fn the_earliest_seal_wins_between_two_absorbs() {
        let seal = |sequence: u64, id: &str, sender: &str, state: &str, at: &str| {
            event(
                sequence,
                id,
                "need-1@aaaaaaaa.local",
                sender,
                "absorbed",
                AT,
                &format!("X-Vivi-Event-At: {at}\nX-Vivi-Event-State: {state}\n"),
            )
        };
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            seal(
                2,
                "late@aaaaaaaa.local",
                "alice@aaaaaaaa.local",
                "open",
                "2026-10-12T12:00:09.000000Z",
            ),
            seal(
                3,
                "early@bbbbbbbb.local",
                BOB,
                "done",
                "2026-10-12T12:00:03.000000Z",
            ),
        ];
        assert_eq!(
            state_of_item(&records, "need-1@aaaaaaaa.local").as_deref(),
            Some("done")
        );
    }

    #[test]
    fn marks_fold_the_newest_letter_per_slug() {
        let mark = |sequence: u64, id: &str, kind: &str, slug: &str, at: &str| {
            event(
                sequence,
                id,
                "need-1@aaaaaaaa.local",
                BOB,
                kind,
                AT,
                &format!("X-Vivi-Event-Slug: {slug}\nX-Vivi-Event-At: {at}\n"),
            )
        };
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            mark(
                2,
                "m1@bbbbbbbb.local",
                "marked",
                "bug",
                "2026-10-12T12:00:01.000000Z",
            ),
            mark(
                3,
                "m2@bbbbbbbb.local",
                "unmarked",
                "bug",
                "2026-10-12T12:00:02.000000Z",
            ),
            mark(
                4,
                "m3@bbbbbbbb.local",
                "marked",
                "bug",
                "2026-10-12T12:00:03.000000Z",
            ),
            mark(
                5,
                "m4@bbbbbbbb.local",
                "marked",
                "regression",
                "2026-10-12T12:00:04.000000Z",
            ),
            // Appended last, stamped before the newest letter for the slug.
            mark(
                6,
                "m5@bbbbbbbb.local",
                "unmarked",
                "regression",
                "2026-10-12T12:00:03.500000Z",
            ),
            mark(
                7,
                "m6@bbbbbbbb.local",
                "marked",
                "gone",
                "2026-10-12T12:00:01.000000Z",
            ),
            mark(
                8,
                "m7@bbbbbbbb.local",
                "unmarked",
                "gone",
                "2026-10-12T12:00:02.000000Z",
            ),
        ];
        let states = fold_item_states(&records);
        assert_eq!(states["need-1@aaaaaaaa.local"].marks, ["bug", "regression"]);
        // Marks do not touch state.
        assert_eq!(states["need-1@aaaaaaaa.local"].state, "open");
    }

    #[test]
    fn a_sealed_item_takes_no_later_marks() {
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "m1@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "marked",
                AT,
                "X-Vivi-Event-Slug: bug\nX-Vivi-Event-At: 2026-10-12T12:00:01.000000Z\n",
            ),
            event(
                3,
                "seal@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "absorbed",
                AT,
                "X-Vivi-Event-State: open\nX-Vivi-Event-At: 2026-10-12T12:00:02.000000Z\n",
            ),
            event(
                4,
                "m2@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "marked",
                AT,
                "X-Vivi-Event-Slug: late\nX-Vivi-Event-At: 2026-10-12T12:00:03.000000Z\n",
            ),
        ];
        let states = fold_item_states(&records);
        assert_eq!(states["need-1@aaaaaaaa.local"].marks, ["bug"]);
    }

    #[test]
    fn the_newest_bind_controls_a_unit() {
        let bind = |sequence: u64, id: &str, of: &str, units: &[&str], at: &str| {
            let units: String = units
                .iter()
                .map(|unit| format!("X-Vivi-Event-Unit: <{unit}>\n"))
                .collect();
            event(
                sequence,
                id,
                of,
                BOB,
                "bound",
                AT,
                &format!("X-Vivi-Event-At: {at}\n{units}"),
            )
        };
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            need(2, "need-2@aaaaaaaa.local"),
            bind(
                3,
                "b1@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                &["unit-a@aaaaaaaa.local", "unit-b@aaaaaaaa.local"],
                "2026-10-12T12:00:05.000000Z",
            ),
            // Appended after, stamped before: moves nothing.
            bind(
                4,
                "b2@bbbbbbbb.local",
                "need-2@aaaaaaaa.local",
                &["unit-a@aaaaaaaa.local"],
                "2026-10-12T12:00:01.000000Z",
            ),
            // Newer: unit-b moves to need-2.
            bind(
                5,
                "b3@bbbbbbbb.local",
                "need-2@aaaaaaaa.local",
                &["unit-b@aaaaaaaa.local"],
                "2026-10-12T12:00:09.000000Z",
            ),
        ];
        let states = fold_item_states(&records);
        assert_eq!(
            states["need-1@aaaaaaaa.local"].units,
            ["unit-a@aaaaaaaa.local"]
        );
        assert_eq!(
            states["need-2@aaaaaaaa.local"].units,
            ["unit-b@aaaaaaaa.local"]
        );
    }

    #[test]
    fn unknown_event_kinds_and_unpayloaded_events_are_skipped() {
        let records = [
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "ev-1@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "snoozed",
                AT,
                "",
            ),
            // marked with no slug, bound with no unit, absorbed with no state
            event(
                3,
                "ev-2@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "marked",
                AT,
                "",
            ),
            event(
                4,
                "ev-3@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "bound",
                AT,
                "",
            ),
            event(
                5,
                "ev-4@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "absorbed",
                AT,
                "",
            ),
        ];
        assert!(fold_item_states(&records).is_empty());
    }

    #[test]
    fn an_event_letter_is_never_an_item_status() {
        let item = need(1, "need-1@aaaaaaaa.local");
        let done = event(
            2,
            "ev-1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            AT,
            "",
        );
        let states = fold_item_states(&[
            need(1, "need-1@aaaaaaaa.local"),
            event(
                2,
                "ev-1@bbbbbbbb.local",
                "need-1@aaaaaaaa.local",
                BOB,
                "done",
                AT,
                "",
            ),
        ]);
        assert_eq!(
            item_status(&item, &parse_letter(&item.raw_eml), &states),
            "done"
        );
        // The event carries the item's envelope kind and is still no item.
        assert_eq!(done.kind.as_deref(), Some("need"));
        assert_eq!(
            item_status(&done, &parse_letter(&done.raw_eml), &states),
            "event"
        );
    }
}
