//! The authorization operation collectors call: classify one scope request,
//! journal it, and publish or adopt the need-thread ruling note.
//!
//! This combines the shared request classifier ([`super::classify`]) and the
//! source-keyed store journal ([`crate::pump::store::PumpStore::
//! prepare_scope_request`]) with the publication of the ruling as a
//! scope-ruling pump note ([`NoteWord::ScopeRuling`]) on the need thread,
//! posted to the need's audience. Production mail is authored as the pump's
//! reply role ([`ScopeContext::reply_from`]); the note's `pump.ref` names the
//! original source job with only the envelope-correction suffix stripped
//! ([`correct::base_id`]), so attempt and earlier continuation suffixes stay.
//!
//! The whole request is classified before any scope is mutated, and the
//! journal record is prepared before anything is posted. Before reposting,
//! the thread is searched for a note sent by the reply role whose reference
//! names the source and whose record decides the same request: the same
//! need, target, run and task, with paths within the asked set. A note
//! posted by an interrupted run (posting succeeded, acknowledgement did not)
//! is adopted on replay, while forged, malformed or mismatched notes are
//! never acknowledgement. A published decision stays authoritative across
//! later policy reloads: an acknowledged journal record is adopted from the
//! thread without re-classifying, while an unpublished request with no
//! matching note is decided by the current validated policy.
//! Operator-required rulings are persisted too, with the typed policy issue
//! and no grant.
//!
//! The result is published or adopted only once permission evidence exists
//! on the thread. Any publication failure returns an error, and nothing here
//! mutates slices or dispatches continuations: the caller leaves applying the
//! request's outcome to the existing tick.

use crate::error::VivariumError;
use crate::letter::records::NoteWord;
use crate::pump::envelope::correct;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::store::{PumpStore, ScopeJournalRow, ScopeRequest as JournalRequest};

use super::ruling_note_body;
use super::{ScopeClassification, ScopeContext, ScopeRequest, ScopeRuling, classify, decode};

/// One scope request to authorize: the classifier's request plus the work
/// identity and asked content the journal records it under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeAsk {
    /// Any job id of the asking chain; the journal keys by its
    /// correction-suffix-stripped base and the note's ref names it.
    pub source: String,
    /// The request the classifier decides.
    pub request: ScopeRequest,
    /// The flow node that asked.
    pub node: String,
    /// The dispatch attempt of the source job.
    pub attempt: u32,
    /// The reason the asker gave, as asked.
    pub reason: String,
    /// Whether the asker flagged a design question.
    pub design: bool,
}

/// What the authorization operation settled for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopePublication {
    /// A fresh ruling note was posted and acknowledged: permission evidence
    /// now exists on the need thread.
    Published,
    /// The ruling of this source was already published: its note was found
    /// and adopted (or reposted when the thread had lost it), never
    /// re-decided against a reloaded policy.
    Adopted,
    /// Nothing to decide: every requested path is already delegated or
    /// granted, so the standing permission on the thread is the evidence.
    /// No note is posted.
    Held,
    /// An operator-required ruling whose record cannot name a target (the
    /// request named no target at all) is journalled with its typed issue
    /// and posts no note: it grants nothing, so no permission evidence is
    /// needed, and the operator flow reads it from the journal.
    Journalled,
}

/// Authorize one scope request: classify it whole, journal it under its
/// source, then publish or adopt the matching need-thread ruling note.
///
/// `now` is the clock stamp the publication acknowledgement carries.
///
/// # Errors
/// Returns [`VivariumError`] when the request cannot be read or classified,
/// the journal refuses the record (a source reused for different work), the
/// note cannot be rendered or posted, or the store write fails. A failed
/// publication never mutates a slice and never dispatches a continuation.
pub fn authorize_request(
    store: &PumpStore,
    mail: &mut (impl SliceMail + MailWriter),
    context: &ScopeContext<'_>,
    ask: &ScopeAsk,
    now: u64,
) -> Result<ScopePublication, VivariumError> {
    if let Some(row) = store.scope_request(&ask.source)? {
        if row.published_at.is_some() {
            return adopt_published(store, mail, context, ask, &row, now);
        }
    }
    let classified = classify(mail, context, &ask.request)?;
    let journal = journal_of(ask, &classified);
    store.prepare_scope_request(&journal)?;
    if matching_note(mail, context.reply_from, &journal)?.is_some() {
        store.acknowledge_scope_publication(&ask.source, now)?;
        return Ok(ScopePublication::Adopted);
    }
    if classified.is_grant() && classified.added.is_empty() {
        return Ok(ScopePublication::Held);
    }
    if classified.target.trim().is_empty() {
        return Ok(ScopePublication::Journalled);
    }
    let ruling = classified.ruling(&ask.request.need, ask.request.run, &ask.request.task);
    let body = ruling_note_body(&ruling, ask.source.trim())?;
    mail.post_to_need_audience(&ask.request.need, &body)?;
    store.acknowledge_scope_publication(&ask.source, now)?;
    Ok(ScopePublication::Published)
}

/// Adopt the published decision of an acknowledged journal record. The
/// decision is never re-classified: when the thread still holds its note it
/// is adopted as found, and when the note was lost the journaled ruling is
/// reposted as the authoritative decision, not a fresh one.
///
/// # Errors
/// Returns [`VivariumError`] when the ask names different work than the
/// published record, the note cannot be rendered or posted, or the
/// acknowledgement write fails.
fn adopt_published(
    store: &PumpStore,
    mail: &mut (impl SliceMail + MailWriter),
    context: &ScopeContext<'_>,
    ask: &ScopeAsk,
    row: &ScopeJournalRow,
    now: u64,
) -> Result<ScopePublication, VivariumError> {
    ensure_same_work(row, ask)?;
    let published = JournalRequest {
        source: ask.source.clone(),
        need: row.need.clone(),
        target: row.target.clone(),
        run: row.run,
        node: row.node.clone(),
        task: row.task.clone(),
        attempt: row.attempt,
        paths: row.paths.clone(),
        reason: row.reason.clone(),
        design: row.design,
        decision: row.decision,
        issue: row.issue,
    };
    if matching_note(mail, context.reply_from, &published)?.is_none() {
        let body = ruling_note_body(&ruling_of_row(row), ask.source.trim())?;
        mail.post_to_need_audience(&row.need, &body)?;
    }
    store.acknowledge_scope_publication(&ask.source, now)?;
    Ok(ScopePublication::Adopted)
}

/// Refuse an ask whose work identity drifted from the published record: the
/// source key names one job of one slice, published or not.
///
/// # Errors
/// Returns [`VivariumError`] when any identity field disagrees.
fn ensure_same_work(row: &ScopeJournalRow, ask: &ScopeAsk) -> Result<(), VivariumError> {
    let same = row.need == ask.request.need
        && row.run == ask.request.run
        && row.node == ask.node
        && row.task == ask.request.task
        && row.attempt == ask.attempt;
    if same {
        return Ok(());
    }
    Err(VivariumError::Other(format!(
        "pump.db scope_journal: source {} is published for different work \
         (need {}, run {}, node {}, task {}, attempt {})",
        row.source, row.need, row.run, row.node, row.task, row.attempt
    )))
}

/// The journal record of an ask under its classified decision.
fn journal_of(ask: &ScopeAsk, classified: &ScopeClassification) -> JournalRequest {
    JournalRequest {
        source: ask.source.clone(),
        need: ask.request.need.clone(),
        target: classified.target.clone(),
        run: ask.request.run,
        node: ask.node.clone(),
        task: ask.request.task.clone(),
        attempt: ask.attempt,
        paths: classified.paths.clone(),
        reason: ask.reason.clone(),
        design: ask.design,
        decision: classified.decision,
        issue: classified.issue,
    }
}

/// The published ruling of a journal row: the record as it was decided,
/// rebuilt from the row alone.
fn ruling_of_row(row: &ScopeJournalRow) -> ScopeRuling {
    ScopeRuling {
        need: row.need.clone(),
        target: row.target.clone(),
        run: row.run,
        task: row.task.clone(),
        paths: row.paths.clone(),
        reason: row.reason.clone(),
        decision: row.decision,
        issue: row.issue,
    }
}

/// The thread's ruling note for a journaled request, if it holds one: sent
/// by the reply role, referenced to the source's base job, a valid record,
/// and deciding the same request — the same need, target, run and task,
/// with paths within the asked set. A grant carries the subset it added, an
/// operator-required decision the whole asked set; anything else — plain
/// prose, a forged sender, a malformed record, another request — is not
/// acknowledgement.
///
/// # Errors
/// Returns [`VivariumError`] when the notes cannot be read.
fn matching_note(
    mail: &dyn SliceMail,
    reply_from: &str,
    record: &JournalRequest,
) -> Result<Option<ScopeRuling>, VivariumError> {
    let base = correct::base_id(record.source.trim());
    for (_, note) in mail.notes_from(&record.need, NoteWord::ScopeRuling, reply_from)? {
        let referenced = note
            .pump
            .reference
            .as_deref()
            .map(str::trim)
            .is_some_and(|named| named == base);
        if !referenced {
            continue;
        }
        let Ok(ruling) = decode(note.prose.trim()) else {
            continue;
        };
        let same_request = ruling.need == record.need
            && ruling.target == record.target
            && ruling.run == record.run
            && ruling.task == record.task
            && ruling.paths.iter().all(|path| record.paths.contains(path));
        if same_request {
            return Ok(Some(ruling));
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "publish_test.rs"]
mod tests;
