//! Durable evidence and atomic delivery for mind decision letters.
//!
//! A mind decision send marks every copy of its letter with one transport
//! header, [`DECISION_MARKER_HEADER`], whose value [`decision_marker`]
//! derives from the decision's stable canonical key. The derivation is
//! deterministic, so the copies one send left behind can be found again on
//! any later tick or restart — including a crash between the ingest steps.
//! [`evidence`] holds that discovery: from durable mail state alone it
//! reports whether the mind recipient's copy, the sender's sent copy, the
//! step-parent link, and the required send events are all present.
//! [`Mailspace::send_mind_decision`] is the decision-specific send: it
//! rechecks the evidence inside one write transaction, lands every part of
//! the delivery whole or not at all, and treats a notice whose evidence is
//! already complete as a success that stores nothing. Ordinary
//! [`Mailspace::send`](crate::mailspace::Mailspace::send) is unchanged.

// The pump decision phase that consumes this send lands next; until that
// caller exists only the companion tests read these items.
#![cfg_attr(not(test), allow(dead_code))]
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use chrono::Utc;

use super::super::Mailspace;
use super::super::event_log::log_send_events;
use super::super::reply::resolve_reply_parent;
use super::ingest::{ingest_for_recipients, ingest_sent_copy};
use crate::error::VivariumError;
use crate::mailspace::SendRequest;
use crate::storage::{Storage, StoredMessageView};

mod evidence;

#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use evidence::{DecisionCopy, DecisionEvents, DecisionEvidence, find_decision_evidence};

/// The transport header that marks every copy of a mind decision letter.
pub(crate) const DECISION_MARKER_HEADER: &str = "X-Vivi-Mind-Decision";

/// The marker for a decision's stable canonical key: the SHA-256 of the
/// trimmed key bytes, hex-encoded. The same key always yields the same
/// marker — every copy of one decision letter carries one identical value,
/// so evidence matching is exact — and a fixed-length hex value never folds,
/// wraps, or leaks the key itself into a transport header.
///
/// # Errors
/// Returns an error when the key is empty or only whitespace: a marker
/// without a decision behind it would match nothing and mean nothing.
pub(crate) fn decision_marker(decision_key: &str) -> Result<String, VivariumError> {
    let key = decision_key.trim();
    if key.is_empty() {
        return Err(VivariumError::Message(
            "a mind decision marker needs a non-empty decision key".into(),
        ));
    }
    Ok(hex::encode(Sha256::digest(key.as_bytes())))
}

/// The local seat every mind decision notice is addressed to.
const MIND_SEAT: &str = "mind";

/// The folder a delivered decision notice is filed under in the seat's
/// mailbox, and the role the send events carry.
const NOTICE_ROLE: &str = "inbox";

/// One mind decision delivery: the handles of the two stored copies and
/// whether the notice had already been fully delivered.
#[derive(Debug)]
pub(crate) struct MindDecisionDelivery {
    /// The mind seat's copy of the notice.
    pub(crate) recipient_handle: String,
    /// The sender's own sent copy.
    pub(crate) sent_handle: String,
    /// Whether a complete notice was already on file and this call stored
    /// nothing new.
    pub(crate) already_complete: bool,
}

/// Where a delivery may be cut short on purpose. [`Seam::Live`] is the only
/// production value; the test variant arms the deterministic failure seam
/// between recipient ingestion and sent-copy creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seam {
    Live,
    #[cfg_attr(not(test), allow(dead_code))]
    FailAfterRecipient,
}

impl Mailspace {
    /// Deliver one mind decision notice, atomically and idempotently.
    ///
    /// The notice is composed to the local seat named `mind` — and to no
    /// other recipient, with empty Cc and Bcc — as a reply to `step` (the
    /// affected step's handle) and marked with [`DECISION_MARKER_HEADER`],
    /// derived from `decision_key`. The seat must be a role of kind `mind`;
    /// an absent seat and a seat of any other kind, operator included, are
    /// both refused, with no fallback.
    ///
    /// Everything the delivery needs stored — the mind seat's copy, the
    /// sender's sent copy, the step-content link, and the send events —
    /// lands inside one write transaction that rechecks the marker evidence
    /// first, so a failure at any stage, commit included, leaves no partial
    /// notice and a retry completes the delivery whole. A notice whose
    /// evidence is already complete is a success that stores nothing;
    /// incomplete evidence is never passed off as success — the send runs
    /// and lands a fresh complete delivery.
    ///
    /// # Errors
    /// Returns an error for an empty decision key, a missing or non-mind
    /// seat, an unknown sender or step handle, an invalid letter body, or a
    /// storage failure.
    pub(crate) fn send_mind_decision(
        &self,
        sender: &str,
        step: &str,
        decision_key: &str,
        body: &str,
    ) -> Result<MindDecisionDelivery, VivariumError> {
        self.deliver_mind_decision(sender, step, decision_key, body, Seam::Live)
    }

    /// Test-only: run the delivery with the deterministic failure seam armed,
    /// stopping it after the mind seat's copy is ingested and before the sent
    /// copy is created. The caller sees the error and an unchanged store.
    ///
    /// # Errors
    /// The same errors [`Mailspace::send_mind_decision`] returns, and always
    /// the seam's own error once the recipient copy is stored.
    #[cfg(test)]
    fn send_mind_decision_failing_after_recipient(
        &self,
        sender: &str,
        step: &str,
        decision_key: &str,
        body: &str,
    ) -> Result<MindDecisionDelivery, VivariumError> {
        self.deliver_mind_decision(sender, step, decision_key, body, Seam::FailAfterRecipient)
    }

    /// Resolve, validate, and compose the notice, then deliver it inside one
    /// write transaction. Everything that can be refused cheaply is refused
    /// before the transaction opens.
    fn deliver_mind_decision(
        &self,
        sender: &str,
        step: &str,
        decision_key: &str,
        body: &str,
        seam: Seam,
    ) -> Result<MindDecisionDelivery, VivariumError> {
        let marker = decision_marker(decision_key)?;
        let seat = self.mind_seat()?;
        let from = self.resolve_identity(sender)?;
        let gated = super::compose::gate(self, Some("mail"), NOTICE_ROLE, body)?;
        let storage = self.storage()?;
        let (parent, parent_bytes) = resolve_reply_parent(&storage, step)?;
        let request = SendRequest {
            from: from.clone(),
            to: vec![seat.clone()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("mind decision: {decision_key}"),
            body: gated.body.clone(),
            role: NOTICE_ROLE.into(),
            kind: Some("mail".into()),
            reply_to: Some(step.into()),
        };
        let (recipients, remote) = self.split_recipients(&request.to, &request.cc)?;
        if recipients.len() != 1 || !remote.is_empty() {
            return Err(VivariumError::Message(
                "a mind decision notice is delivered to the local mind seat alone".into(),
            ));
        }
        let eml = self.compose_message(
            &from,
            &request,
            Some(parent_bytes.as_slice()),
            &[(DECISION_MARKER_HEADER.into(), marker)],
            &gated,
        )?;
        self.check_letter_size(eml.len())?;
        in_atomic_batch(&storage, || {
            let evidence = find_decision_evidence(&storage, decision_key, &from, &seat)?;
            if evidence.complete() {
                return Ok(existing_delivery(&evidence));
            }
            deliver_fresh(&storage, &request, &eml, &parent, seam)
        })
    }

    /// Resolve the local seat named `mind` and require its role kind to be
    /// `mind`. There is no fallback: an absent seat and a seat of any other
    /// kind, operator included, are refused.
    ///
    /// # Errors
    /// Returns an error when no `mind` role exists or its kind is not `mind`.
    fn mind_seat(&self) -> Result<String, VivariumError> {
        let seat = self.resolve_identity(MIND_SEAT)?;
        if !self.identity_is_mind(&seat) {
            return Err(VivariumError::Message(format!(
                "the mind decision seat '{MIND_SEAT}' must be a role of kind 'mind'; \
                 a seat of any other kind cannot hold decision notices"
            )));
        }
        Ok(seat)
    }
}

/// Store the whole delivery inside the caller's open batch: the mind seat's
/// copy, the sent copy, the step-content link, and the send events. `seam`
/// is the deterministic test-only failure point after the recipient copy but
/// before the sent copy; the caller rolls the batch back on its error.
///
/// # Errors
/// Returns the seam's error when armed, or a storage failure.
fn deliver_fresh(
    storage: &Storage,
    request: &SendRequest,
    eml: &str,
    parent: &StoredMessageView,
    seam: Seam,
) -> Result<MindDecisionDelivery, VivariumError> {
    let seed = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let delivered =
        ingest_for_recipients(storage, recipients_of(request), &request.role, eml, seed)?;
    if seam == Seam::FailAfterRecipient {
        return Err(VivariumError::Message(
            "test seam: the decision send stopped after the recipient copy".into(),
        ));
    }
    let sent = ingest_sent_copy(storage, &request.from, eml, seed)?;
    storage.link_mailspace_content(&sent.content_id, &parent.content_id, "captured")?;
    log_send_events(
        storage,
        &request.from,
        request,
        &delivered,
        &sent,
        Some(parent),
    )?;
    let first = delivered.first().ok_or_else(|| {
        VivariumError::Message("the mind decision notice was not delivered".into())
    })?;
    Ok(MindDecisionDelivery {
        recipient_handle: storage.display_handle(&first.message_id)?,
        sent_handle: storage.display_handle(&sent.message_id)?,
        already_complete: false,
    })
}

/// The success reported when the durable evidence already shows a complete
/// notice: no new copy, no new event, the found handles passed back.
fn existing_delivery(evidence: &DecisionEvidence) -> MindDecisionDelivery {
    MindDecisionDelivery {
        recipient_handle: evidence
            .recipient
            .as_ref()
            .map_or_else(String::new, |copy| copy.handle.clone()),
        sent_handle: evidence
            .sent
            .as_ref()
            .map_or_else(String::new, |copy| copy.handle.clone()),
        already_complete: true,
    }
}

/// The local recipients of a request, deduplicated.
fn recipients_of(request: &SendRequest) -> BTreeSet<String> {
    request.to.iter().cloned().collect()
}

/// Run `work` inside one store write transaction: committed when it succeeds,
/// rolled back when it fails — a failed commit included — so a refused or
/// interrupted stage leaves the mailspace as it was and concurrent writers
/// are serialized.
fn in_atomic_batch<T>(
    storage: &Storage,
    work: impl FnOnce() -> Result<T, VivariumError>,
) -> Result<T, VivariumError> {
    storage.begin_batch()?;
    match work() {
        Ok(value) => match storage.commit_batch() {
            Ok(()) => Ok(value),
            Err(commit) => Err(rolled_back(storage, commit)),
        },
        Err(error) => Err(rolled_back(storage, error)),
    }
}

/// Roll the open batch back and attach the rollback's outcome to `error`.
fn rolled_back(storage: &Storage, error: VivariumError) -> VivariumError {
    match storage.rollback_batch() {
        Ok(()) => error,
        Err(rollback) => {
            VivariumError::Message(format!("{error}; the rollback also failed: {rollback}"))
        }
    }
}

#[cfg(test)]
#[path = "mind_decision_test.rs"]
mod tests;
