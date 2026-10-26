//! Run thread inputs: durable identity snapshots of a run's reply stream.
//!
//! One representation serves every reader of a run's input: the baseline a
//! start journals, the brief a resumed source is shown, and the thread-change
//! check. A reply keeps its message id, author, body and thread position;
//! pump-authored progress, lifecycle events, pump record letters and action
//! letters are not replies. The message id alone is the identity: distinct
//! replies sent in the same second stay distinct snapshots, and a timestamp
//! is evidence, never a consumption identity.

use std::collections::BTreeSet;

use chrono::DateTime;

use crate::error::VivariumError;
use crate::letter::prose_of;
use crate::mailspace::MailspaceThreadMessage;
use crate::pump::store::PumpStore;
use crate::pump::store::flow_continuation::ThreadInput;

/// One reply on a thread, kept at its thread position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSnapshot {
    /// The message's position on the thread, 0-based, counted over every
    /// message: filtering never reorders or renumbers the survivors.
    pub position: usize,
    /// The durable identity snapshot of the message.
    pub input: ThreadInput,
}

/// The replies on `thread`, in thread order, as identity snapshots. A message
/// the pump authored, a lifecycle event, a pump record and a posted action
/// letter are not replies. `pump` is the pump identity as it appears in the
/// message's `from`.
///
/// # Errors
/// Returns a [`VivariumError`] when a thread date does not parse.
pub fn snapshots(
    thread: &[MailspaceThreadMessage],
    pump: &str,
) -> Result<Vec<ThreadSnapshot>, VivariumError> {
    let mut found = Vec::new();
    for (position, message) in thread.iter().enumerate() {
        if message.event.is_some() || message.from == pump || is_pump_letter(message) {
            continue;
        }
        found.push(ThreadSnapshot {
            position,
            input: ThreadInput {
                message_id: message.message_id.clone(),
                author: message.from.clone(),
                body: message.body.clone(),
                sent_at: epoch(&message.date)?,
            },
        });
    }
    Ok(found)
}

/// The run's journalled input baseline, or `None` when the run has none: the
/// durable record of what the run was shown when it began.
///
/// # Errors
/// Returns a [`VivariumError`] when the journal cannot be read.
pub fn baseline(
    store: &PumpStore,
    need: &str,
    run: u32,
) -> Result<Option<Vec<ThreadInput>>, VivariumError> {
    let inputs = store.run_inputs(need, run)?;
    Ok((!inputs.is_empty()).then_some(inputs))
}

/// The replies the run has not been shown yet, in thread order. With a
/// baseline, the snapshots whose message id the baseline lacks. Without one —
/// a run started before baselines existed — the replies after the run's start
/// post, the durable evidence on the thread; earlier replies are left alone
/// and nothing is claimed consumed. With neither baseline nor start post the
/// boundary cannot be established safely: an error for
/// flow-thread-check-failed.
///
/// # Errors
/// Returns a [`VivariumError`] when the journal cannot be read, a thread
/// date does not parse, or the boundary cannot be established safely.
pub fn new_inputs(
    thread: &[MailspaceThreadMessage],
    pump: &str,
    store: &PumpStore,
    need: &str,
    run: u32,
) -> Result<Vec<ThreadSnapshot>, VivariumError> {
    let all = snapshots(thread, pump)?;
    match baseline(store, need, run)? {
        Some(baseline) => {
            let shown: BTreeSet<&str> = baseline
                .iter()
                .map(|input| input.message_id.as_str())
                .collect();
            Ok(all
                .into_iter()
                .filter(|snapshot| !shown.contains(snapshot.input.message_id.as_str()))
                .collect())
        }
        None => match start_post_position(thread, pump, run) {
            Some(start) => Ok(all
                .into_iter()
                .filter(|snapshot| snapshot.position > start)
                .collect()),
            None => Err(VivariumError::Other(format!(
                "need {need} run r{run}: no journalled baseline and no start post on the thread; \
                 the input boundary cannot be established safely (flow-thread-check-failed)"
            ))),
        },
    }
}

/// The thread position of the run's start post, when the thread carries one.
fn start_post_position(thread: &[MailspaceThreadMessage], pump: &str, run: u32) -> Option<usize> {
    let marker = format!("pump: run r{run} started");
    thread.iter().position(|message| {
        message.from == pump
            && message.event.is_none()
            && prose_of(&message.body).starts_with(&marker)
    })
}

/// A pump record or a posted action letter, never a reply on the thread.
fn is_pump_letter(message: &MailspaceThreadMessage) -> bool {
    message.kind.as_deref() == Some("pump")
        || (message.subject.starts_with("pump ") && message.subject.ends_with(" action"))
}

/// The unix seconds of an RFC 3339 thread date. Evidence only: never part of
/// an identity, so an unparsable date is an error and a pre-epoch date
/// saturates at zero.
fn epoch(date: &str) -> Result<u64, VivariumError> {
    let parsed = DateTime::parse_from_rfc3339(date)
        .map_err(|error| VivariumError::Parse(format!("thread date {date:?}: {error}")))?;
    Ok(u64::try_from(parsed.timestamp()).unwrap_or(0))
}
