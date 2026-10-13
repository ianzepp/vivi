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
use crate::storage::{Storage, StoredMessageView, is_traveling_id};
use crate::sync::filer::forged_from;
use crate::sync::link;
use crate::sync::{MAX_LETTER_BYTES, PROVIDER, SyncSettings};

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
    /// unsent until someone acts.
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
}

/// Whether a stored date (RFC 3339) is strictly before `start`. An empty or
/// unreadable date is not before anything.
fn dated_before(date: &str, start: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(date).is_ok_and(|date| date.with_timezone(&Utc) < start)
}

/// Choose the letters to push, oldest first.
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
        match classify(storage, &rows, settings.address_prefix())? {
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

fn excluded_role(role: &str) -> Option<SkipReason> {
    match role {
        "memos" => Some(SkipReason::Memo),
        "drafts" => Some(SkipReason::Draft),
        "trash" => Some(SkipReason::Trash),
        _ => None,
    }
}

/// Pick the row that names the letter on the wire and decide whether it
/// goes. The id printed to the sender is the delivered row's, so a non-`sent`
/// row wins; the sent copy names a letter only when nothing else holds it.
fn classify(
    storage: &Storage,
    rows: &[StoredMessageView],
    own_prefix: &str,
) -> Result<Result<Letter, Skipped>, VivariumError> {
    let skip = |row: &StoredMessageView, reason| {
        Ok(Err(Skipped {
            handle: row.handle.clone(),
            reason,
        }))
    };
    let sendable: Vec<&StoredMessageView> = rows
        .iter()
        .filter(|row| excluded_role(&row.local_role).is_none())
        .collect();
    let Some(first) = sendable.first() else {
        let reason = excluded_role(&rows[0].local_role).unwrap_or(SkipReason::Trash);
        return skip(&rows[0], reason);
    };
    let mut free: Vec<&StoredMessageView> = sendable
        .iter()
        .copied()
        .filter(|row| row.remote.is_none())
        .collect();
    free.sort_by(|a, b| {
        (a.local_role == "sent", &a.message_id).cmp(&(b.local_role == "sent", &b.message_id))
    });
    let Some(row) = free.first().copied() else {
        let remote = first.remote.as_ref().map_or_else(String::new, |remote| {
            format!("{} {}", remote.provider, remote.remote_mailbox)
        });
        return skip(first, SkipReason::BoundElsewhere(remote));
    };
    if row.byte_size > MAX_LETTER_BYTES {
        return skip(row, SkipReason::TooLarge(row.byte_size));
    }
    if !is_traveling_id(&row.message_id) {
        return skip(row, SkipReason::NotTravelingId(row.message_id.clone()));
    }
    let data = storage.read_message(&row.message_id)?;
    let headers = link::parse_headers(&data);
    if let Some(reason) = forged_from(Some(own_prefix), &headers) {
        return skip(row, SkipReason::ForeignFrom(reason));
    }
    let events = storage.list_mailspace_events(&row.message_id)?;
    let kind = match effective_kind(&row.local_role, &data, &events).as_deref() {
        None | Some("mail") => "mail".to_string(),
        Some(kind @ ("need" | "task" | "want")) => kind.to_string(),
        Some(other) => return skip(row, SkipReason::OtherKind(other.to_string())),
    };
    Ok(Ok(Letter {
        wire_id: row.message_id.clone(),
        handle: row.handle.clone(),
        content_id: row.content_id.clone(),
        account: row.account.clone(),
        kind,
        to: headers.envelope_to(),
    }))
}
