//! Push selection: which local letters still need to go to the server.
//!
//! Rows are grouped by `content_id`: one letter, however many local rows
//! (delivered copies, the sent copy) hold it. A letter is already pushed when
//! any of its rows is bound to this project. Memos, drafts, trash, and any
//! kind other than need, task, want, or plain mail never travel. A letter the
//! server or every puller would refuse (over the size cap, or a From that
//! claims another mailspace) is skipped and reported, not uploaded.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::error::VivariumError;
use crate::mailspace::effective_kind;
use crate::message::normalize_message_id;
use crate::storage::{Storage, StoredMessageView, is_traveling_id};
use crate::sync::filer::forged_from;
use crate::sync::kept::KeptLocal;
use crate::sync::link::{self, LetterHeaders};
use crate::sync::{MAX_LETTER_BYTES, PROVIDER, SendFilter, SyncSettings};

/// One letter to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Letter {
    /// The id printed to the sender: the wire id.
    pub wire_id: String,
    pub handle: String,
    pub content_id: String,
    /// Account of the row the wire id names; the binding is written on it.
    pub account: String,
    /// `need`, `task`, `want`, or `mail`.
    pub kind: String,
    /// Declared recipients: the letter's own To and Cc addresses, bare and
    /// lowercased, deduplicated, sorted, joined with `,`. Empty means the
    /// header is omitted.
    pub to: String,
}

/// Why a letter is not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Memo,
    Draft,
    Trash,
    /// An effective kind that does not travel (rule, or an unknown one).
    OtherKind(String),
    TooLarge(u64),
    /// The From address claims another mailspace's prefix. Every puller
    /// refuses such a letter, so it is not uploaded. Holds the reason, in
    /// the words the pull side uses.
    ForeignFrom(String),
    NotTravelingId(String),
    /// Every sendable row already holds a binding to another remote.
    BoundElsewhere(String),
    /// The operator recorded the letter as kept local: push no longer
    /// tries it.
    KeptLocal,
    /// `send = "addressed"` and nobody off this machine is on the letter:
    /// machine-local traffic (tasks to its own hands, receipts, worker
    /// mail). Policy, like a memo: only counted, never unsent.
    LocalOnly,
}

impl SkipReason {
    /// Whether the operator can act on this skip. Memos, drafts, and trash
    /// are policy and only counted.
    #[must_use]
    pub fn actionable(&self) -> bool {
        matches!(
            self,
            Self::TooLarge(_)
                | Self::ForeignFrom(_)
                | Self::NotTravelingId(_)
                | Self::BoundElsewhere(_)
        )
    }

    /// Whether the letter can never be delivered as it is: the server would
    /// refuse it or every puller would. Push reports these as failures.
    #[must_use]
    pub fn undeliverable(&self) -> bool {
        matches!(self, Self::TooLarge(_) | Self::ForeignFrom(_))
    }

    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Memo => "memo".into(),
            Self::Draft => "draft".into(),
            Self::Trash => "trash".into(),
            Self::OtherKind(kind) => format!("kind {kind}"),
            Self::TooLarge(bytes) => format!("{bytes} bytes, over the 64 KiB cap; not uploaded"),
            Self::ForeignFrom(reason) => format!("{reason}; not uploaded"),
            Self::NotTravelingId(id) => format!("id {id} is not a traveling id"),
            Self::BoundElsewhere(remote) => format!("already bound to {remote}"),
            Self::KeptLocal => "kept local".into(),
            Self::LocalOnly => "local only (send = addressed)".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub handle: String,
    pub reason: SkipReason,
}

#[derive(Debug, Default)]
pub struct Selection {
    pub letters: Vec<Letter>,
    pub skipped: Vec<Skipped>,
    /// Letters a push would send that are dated before the push window. They
    /// stay unsent for a push without a window.
    pub held_back: usize,
}

impl Selection {
    /// Letters waiting to go: those a push would send, plus those it cannot
    /// (too large, or a From that claims another mailspace), which stay
    /// unsent until someone acts. A letter kept local is settled by design
    /// and not waiting.
    #[must_use]
    pub fn unsent(&self) -> usize {
        let stuck = self.skipped.iter().filter(|s| s.reason.undeliverable());
        self.letters.len() + stuck.count() + self.held_back
    }

    /// One line counting the actionable skips by reason, or `None` when a
    /// push would skip nothing it could report.
    #[must_use]
    pub fn not_uploaded_line(&self) -> Option<String> {
        let kinds: [(&str, fn(&SkipReason) -> bool); 4] = [
            ("too large (over the 64 KiB cap)", |r| {
                matches!(r, SkipReason::TooLarge(_))
            }),
            ("with a From that claims another mailspace", |r| {
                matches!(r, SkipReason::ForeignFrom(_))
            }),
            ("bound to another remote", |r| {
                matches!(r, SkipReason::BoundElsewhere(_))
            }),
            ("without a traveling id", |r| {
                matches!(r, SkipReason::NotTravelingId(_))
            }),
        ];
        let parts: Vec<String> = kinds
            .iter()
            .filter_map(|(what, is)| {
                let count = self.skipped.iter().filter(|s| is(&s.reason)).count();
                (count > 0).then(|| format!("{count} {what}"))
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// The stuck letters: those that can never be delivered as they are
    /// (too large, or a From that claims another mailspace), as handle and
    /// label — the letters `vivi sync keep-local` can clear.
    #[must_use]
    pub fn stuck(&self) -> Vec<(&str, String)> {
        self.skipped
            .iter()
            .filter(|skipped| skipped.reason.undeliverable())
            .map(|skipped| (skipped.handle.as_str(), skipped.reason.label()))
            .collect()
    }
}

/// Whether a stored date (RFC 3339) is strictly before `start`. An empty or
/// unreadable date is not before anything.
fn dated_before(date: &str, start: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(date).is_ok_and(|date| date.with_timezone(&Utc) < start)
}

/// Choose the letters to push, oldest first. A letter whose content id is
/// kept local is skipped as [`SkipReason::KeptLocal`] instead of classified:
/// push no longer tries it.
///
/// With a `window`, a letter dated before it is not sent: one a push would
/// have sent is counted in `held_back`, and one a push would have skipped is
/// left out of the report. A letter whose date cannot be read is never held
/// back: its age is unknown, so it travels.
///
/// # Errors
/// Returns an error on a storage failure.
pub fn select(
    storage: &Storage,
    settings: &SyncSettings,
    kept: &KeptLocal,
    window: Option<DateTime<Utc>>,
) -> Result<Selection, VivariumError> {
    let mut groups: BTreeMap<String, Vec<StoredMessageView>> = BTreeMap::new();
    for row in storage.list_messages()? {
        groups.entry(row.content_id.clone()).or_default().push(row);
    }
    let mut groups: Vec<Vec<StoredMessageView>> = groups.into_values().collect();
    groups.sort_by(|a, b| {
        (a[0].date.as_str(), a[0].content_id.as_str())
            .cmp(&(b[0].date.as_str(), b[0].content_id.as_str()))
    });
    let mut selection = Selection::default();
    for rows in groups {
        if rows.iter().any(|row| bound_here(row, &settings.project)) {
            continue;
        }
        let outside = window.is_some_and(|start| dated_before(&rows[0].date, start));
        if kept.contains(&rows[0].content_id) {
            // Kept local is settled by design, not this push's business;
            // outside a window nothing is reported, as for any skip.
            if !outside {
                selection.skipped.push(Skipped {
                    handle: wire_handle(&rows).to_string(),
                    reason: SkipReason::KeptLocal,
                });
            }
            continue;
        }
        match classify(storage, &rows, settings)? {
            Ok(_) if outside => selection.held_back += 1,
            Ok(letter) => selection.letters.push(letter),
            // Outside the window nothing is reported: a letter that would be
            // skipped is not this push's business.
            Err(_) if outside => {}
            Err(skipped) => selection.skipped.push(skipped),
        }
    }
    Ok(selection)
}

fn bound_here(row: &StoredMessageView, project: &str) -> bool {
    row.remote
        .as_ref()
        .is_some_and(|remote| remote.provider == PROVIDER && remote.remote_mailbox == project)
}

/// The row that names a kept letter: the first row no role excludes, else
/// the first row — the same row [`classify`] would name first.
fn wire_handle(rows: &[StoredMessageView]) -> &str {
    rows.iter()
        .find(|row| excluded_role(&row.local_role).is_none())
        .unwrap_or(&rows[0])
        .handle
        .as_str()
}

fn excluded_role(role: &str) -> Option<SkipReason> {
    match role {
        "memos" => Some(SkipReason::Memo),
        "drafts" => Some(SkipReason::Draft),
        "trash" => Some(SkipReason::Trash),
        _ => None,
    }
}

/// Pick the row that names the letter on the wire, or why none does: the
/// delivered row's id is the wire id, so a non-`sent` row wins; the sent
/// copy names a letter only when nothing else holds it. Every row held in
/// a role that never travels, or every sendable row already bound to
/// another remote, is a skip.
fn naming_row(rows: &[StoredMessageView]) -> Result<&StoredMessageView, Skipped> {
    let sendable: Vec<&StoredMessageView> = rows
        .iter()
        .filter(|row| excluded_role(&row.local_role).is_none())
        .collect();
    let Some(first) = sendable.first() else {
        let reason = excluded_role(&rows[0].local_role).unwrap_or(SkipReason::Trash);
        return Err(Skipped {
            handle: rows[0].handle.clone(),
            reason,
        });
    };
    let mut free: Vec<&StoredMessageView> = sendable
        .iter()
        .copied()
        .filter(|row| row.remote.is_none())
        .collect();
    free.sort_by(|a, b| {
        (a.local_role == "sent", &a.message_id).cmp(&(b.local_role == "sent", &b.message_id))
    });
    free.first().copied().ok_or_else(|| {
        let remote = first.remote.as_ref().map_or_else(String::new, |remote| {
            format!("{} {}", remote.provider, remote.remote_mailbox)
        });
        Skipped {
            handle: first.handle.clone(),
            reason: SkipReason::BoundElsewhere(remote),
        }
    })
}

/// Pick the row that names the letter on the wire and decide whether it
/// goes. The id printed to the sender is the delivered row's, so a non-`sent`
/// row wins; the sent copy names a letter only when nothing else holds it.
fn classify(
    storage: &Storage,
    rows: &[StoredMessageView],
    settings: &SyncSettings,
) -> Result<Result<Letter, Skipped>, VivariumError> {
    let skip = |row: &StoredMessageView, reason| {
        Ok(Err(Skipped {
            handle: row.handle.clone(),
            reason,
        }))
    };
    let row = match naming_row(rows) {
        Ok(row) => row,
        Err(skipped) => return Ok(Err(skipped)),
    };
    if row.byte_size > MAX_LETTER_BYTES {
        return skip(row, SkipReason::TooLarge(row.byte_size));
    }
    if !is_traveling_id(&row.message_id) {
        return skip(row, SkipReason::NotTravelingId(row.message_id.clone()));
    }
    let data = storage.read_message(&row.message_id)?;
    let headers = link::parse_headers(&data);
    if let Some(reason) = forged_from(Some(settings.address_prefix()), &headers) {
        return skip(row, SkipReason::ForeignFrom(reason));
    }
    let events = storage.list_mailspace_events(&row.message_id)?;
    let kind = match effective_kind(&row.local_role, &data, &events).as_deref() {
        None | Some("mail") => "mail".to_string(),
        Some(kind @ ("need" | "task" | "want")) => kind.to_string(),
        Some(other) => return skip(row, SkipReason::OtherKind(other.to_string())),
    };
    if !leaves_this_machine(storage, settings, &data, &headers)? {
        return skip(row, SkipReason::LocalOnly);
    }
    Ok(Ok(Letter {
        wire_id: row.message_id.clone(),
        handle: row.handle.clone(),
        content_id: row.content_id.clone(),
        account: row.account.clone(),
        kind,
        to: headers.envelope_to(),
    }))
}

/// Whether the letter leaves this machine under `send = "addressed"`: a
/// recipient address in another mailspace's domain names someone off this
/// machine, and an event letter for an item already bound to this project
/// follows work that has already left. Under `send = "all"` every letter
/// goes.
///
/// # Errors
/// Returns an error on a storage failure.
fn leaves_this_machine(
    storage: &Storage,
    settings: &SyncSettings,
    data: &[u8],
    headers: &LetterHeaders,
) -> Result<bool, VivariumError> {
    if settings.send == SendFilter::All {
        return Ok(true);
    }
    if addressed_off_machines(&headers.recipients, settings.address_prefix()) {
        return Ok(true);
    }
    if !headers.is_event {
        return Ok(false);
    }
    follows_bound_item(storage, settings, data)
}

/// Whether any To or Cc address names a domain other than this mailspace's
/// own address domain (or the bare `local`): a recipient on another machine.
/// An address with no domain counts as local.
fn addressed_off_machines(recipients: &[String], own_prefix: &str) -> bool {
    let own = format!("{own_prefix}.local");
    recipients.iter().any(|address| {
        address
            .rsplit_once('@')
            .is_some_and(|(_, domain)| domain != "local" && domain != own)
    })
}

/// Whether `data` is an event letter whose item (`X-Vivi-Event-Of`) is held
/// here with a row bound to this project, so the event follows work that
/// has already left the machine.
///
/// # Errors
/// Returns an error on a storage failure.
fn follows_bound_item(
    storage: &Storage,
    settings: &SyncSettings,
    data: &[u8],
) -> Result<bool, VivariumError> {
    let Some(item_id) = event_of_header(data) else {
        return Ok(false);
    };
    let Some(item) = storage.live_message_by_rfc_id(&item_id)? else {
        return Ok(false);
    };
    Ok(storage
        .live_messages_for_content(&item.content_id)?
        .iter()
        .any(|row| bound_here(row, &settings.project)))
}

/// The normalized `X-Vivi-Event-Of` id of an event letter: the item its
/// event reports on, when the letter names one.
fn event_of_header(data: &[u8]) -> Option<String> {
    let parsed = mail_parser::MessageParser::default().parse(data)?;
    parsed
        .headers()
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case("X-Vivi-Event-Of"))
        .and_then(|header| match header.value() {
            mail_parser::HeaderValue::Text(text) => normalize_message_id(text),
            _ => None,
        })
}
