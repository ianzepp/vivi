//! Discovery of the durable evidence a mind decision send leaves.
//!
//! Every copy of a decision letter carries the
//! [`DECISION_MARKER_HEADER`] marker derived from the decision key. This
//! module answers, from durable mail state alone: is the mind recipient's
//! copy here, is the sender's sent copy here, is the letter linked to its
//! step parent, and did the send log its required events? Each found copy
//! retains its original bytes and copy identity, so an incomplete delivery
//! can be recovered from what a previous attempt actually stored. The scan
//! walks `Storage::list_messages` and the stored bytes — not an inbox-only
//! or sent-only query — so copies are found wherever moves have filed them,
//! and the actual local mind recipient copy stays distinct from the sender's
//! copy even when sender and recipient are both mind.

use crate::error::VivariumError;
use crate::storage::{MailspaceLink, Storage, StoredMessageView};

use super::{DECISION_MARKER_HEADER, decision_marker};

/// The local folder a sender's own copy is filed under.
const SENT_ROLE: &str = "sent";
/// The event type a send logs on the sender's copy.
const SENT_COPY_CREATED: &str = "sent_copy_created";
/// The event type a send logs on each local recipient's copy.
const DELIVERED: &str = "delivered";

/// One discovered copy of a decision letter: its copy identity in this
/// mailspace plus the original bytes, retained for incomplete-delivery
/// recovery.
#[derive(Debug)]
pub(crate) struct DecisionCopy {
    /// The copy's display handle.
    pub(crate) handle: String,
    /// The copy's own storage message id; one per stored row.
    pub(crate) message_id: String,
    /// The content id every copy of the letter shares.
    pub(crate) content_id: String,
    /// The account (local identity) the copy is filed under.
    pub(crate) account: String,
    /// The local folder the copy sits in now: `sent`, or the delivery role
    /// wherever a move has left it.
    pub(crate) local_role: String,
    /// The original letter bytes as stored.
    pub(crate) bytes: Vec<u8>,
}

/// Whether the required send events are on the event log for the discovered
/// copies. A copy without its event is the signature of an interrupted send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DecisionEvents {
    /// `sent_copy_created` on the sender's copy.
    pub(crate) sent_copy_created: bool,
    /// `delivered` on the mind recipient's copy.
    pub(crate) delivered: bool,
}

/// Everything a decision send path can prove from durable mail evidence.
#[derive(Debug)]
pub(crate) struct DecisionEvidence {
    /// The marker the evidence was matched by.
    pub(crate) marker: String,
    /// The copy in the local mind recipient's mailbox, if present.
    pub(crate) recipient: Option<DecisionCopy>,
    /// The sender's own copy, if present.
    pub(crate) sent: Option<DecisionCopy>,
    /// The letter content's step-parent link, if the send captured one.
    pub(crate) step_parent_link: Option<MailspaceLink>,
    /// Whether the required send events are present for both copies.
    pub(crate) required_events: DecisionEvents,
}

impl DecisionEvidence {
    /// Whether the actual local mind recipient copy is present.
    #[must_use]
    pub(crate) fn recipient_present(&self) -> bool {
        self.recipient.is_some()
    }

    /// Whether the sender's copy is present.
    #[must_use]
    pub(crate) fn sent_present(&self) -> bool {
        self.sent.is_some()
    }

    /// Whether the letter content is linked to a step parent.
    #[must_use]
    pub(crate) fn step_parent_link_present(&self) -> bool {
        self.step_parent_link.is_some()
    }

    /// Whether both required send events are on the log.
    #[must_use]
    pub(crate) fn required_events_present(&self) -> bool {
        self.required_events.sent_copy_created && self.required_events.delivered
    }

    /// Whether every part of the durable evidence is in place.
    #[must_use]
    pub(crate) fn complete(&self) -> bool {
        self.recipient_present()
            && self.sent_present()
            && self.step_parent_link_present()
            && self.required_events_present()
    }
}

/// Find the durable evidence one decision send left in this mailspace: the
/// copy in `recipient`'s mailbox, `sender`'s sent copy, the step-parent
/// link, and the required send events — all matched by the
/// [`DECISION_MARKER_HEADER`] value derived from `decision_key`.
///
/// The scan walks every non-deleted stored copy
/// ([`Storage::list_messages`]) and reads its stored bytes, so a match never
/// depends on the copy still sitting in `inbox` or `sent`. The recipient
/// side is a row in `recipient`'s account whose folder is not `sent` (the
/// delivery folder, wherever a move left it); the sender side is a row in
/// `sender`'s account under `sent` — two distinct rows even when sender and
/// recipient are both mind. Rows list newest first, so of several copies of
/// one side the newest wins.
///
/// # Errors
/// Returns an error for an empty decision key or a storage failure.
pub(crate) fn find_decision_evidence(
    storage: &Storage,
    decision_key: &str,
    sender: &str,
    recipient: &str,
) -> Result<DecisionEvidence, VivariumError> {
    let marker = decision_marker(decision_key)?;
    let mut recipient_copy: Option<DecisionCopy> = None;
    let mut sent_copy: Option<DecisionCopy> = None;
    for row in storage.list_messages()? {
        let bytes = storage.read_message(&row.message_id)?;
        if marker_from_bytes(&bytes).as_deref() != Some(marker.as_str()) {
            continue;
        }
        if recipient_copy.is_none() && row.local_role != SENT_ROLE && row.account == recipient {
            recipient_copy = Some(copy(&row, bytes));
        } else if sent_copy.is_none() && row.local_role == SENT_ROLE && row.account == sender {
            sent_copy = Some(copy(&row, bytes));
        }
        if recipient_copy.is_some() && sent_copy.is_some() {
            break;
        }
    }
    let step_parent_link = match recipient_copy.as_ref().or(sent_copy.as_ref()) {
        Some(found) => storage.mailspace_link_for_child(&found.content_id)?,
        None => None,
    };
    let required_events =
        scan_required_events(storage, recipient_copy.as_ref(), sent_copy.as_ref())?;
    Ok(DecisionEvidence {
        marker,
        recipient: recipient_copy,
        sent: sent_copy,
        step_parent_link,
        required_events,
    })
}

/// The [`DECISION_MARKER_HEADER`] value in a stored letter's header block,
/// or `None` when the letter carries no marker. First match wins; header
/// names compare case-insensitively.
fn marker_from_bytes(bytes: &[u8]) -> Option<String> {
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(line);
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break; // End of the header block.
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(DECISION_MARKER_HEADER) {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// The copy identity and original bytes of a matching row.
fn copy(row: &StoredMessageView, bytes: Vec<u8>) -> DecisionCopy {
    DecisionCopy {
        handle: row.handle.clone(),
        message_id: row.message_id.clone(),
        content_id: row.content_id.clone(),
        account: row.account.clone(),
        local_role: row.local_role.clone(),
        bytes,
    }
}

/// Look up the required send events for whichever copies were found.
fn scan_required_events(
    storage: &Storage,
    recipient: Option<&DecisionCopy>,
    sent: Option<&DecisionCopy>,
) -> Result<DecisionEvents, VivariumError> {
    let mut ids = Vec::new();
    if let Some(copy) = recipient {
        ids.push(copy.message_id.clone());
    }
    if let Some(copy) = sent {
        ids.push(copy.message_id.clone());
    }
    if ids.is_empty() {
        return Ok(DecisionEvents::default());
    }
    let events = storage.list_mailspace_events_for_messages(&ids)?;
    let logged = |copy: &DecisionCopy, event_type: &str| {
        events
            .get(&copy.message_id)
            .is_some_and(|found| found.iter().any(|event| event.event_type == event_type))
    };
    Ok(DecisionEvents {
        sent_copy_created: sent.is_some_and(|copy| logged(copy, SENT_COPY_CREATED)),
        delivered: recipient.is_some_and(|copy| logged(copy, DELIVERED)),
    })
}

#[cfg(test)]
#[path = "evidence_test.rs"]
mod tests;
