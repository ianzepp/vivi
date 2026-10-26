//! Recovery input snapshots: the need replies an execution's input included.
//!
//! When the tick registers an execution and enqueues its handler job, it
//! snapshots the need's thread into the execution's input checkpoint: the
//! message IDs of every eligible reply the job's brief was built from. A later
//! execution of the same task reads the checkpoint and knows which replies the
//! earlier executions already consumed; a reply that arrived after the capture
//! is new input for the next capture.
//!
//! Eligibility is decided from message IDs, never from a date boundary:
//! thread dates carry one-second resolution, so a freshness comparison
//! misfiles a reply captured in the same second. The exclusions mirror the
//! thread-change reader: the need's own root letter, lifecycle event letters,
//! mail the pump identity authored, pump record letters and posted operator
//! action letters are not replies. A thread date that does not parse stays a
//! visible error, so a corrupt thread fails the capture loudly instead of
//! silently narrowing the recorded input.
//!
//! The checkpoint is a JSON array of message IDs, oldest first; an execution
//! captured with no eligible reply carries `[]`.

use chrono::DateTime;
use serde_json::to_string;

use crate::error::VivariumError;
use crate::mailspace::{Mailspace, MailspaceThreadMessage, Recipient};
use crate::pump::host::Ctx;
use crate::pump::record::open_mailspace;

/// The checkpoint of an execution captured with no eligible reply.
const EMPTY: &str = "[]";

/// The input checkpoint for a capture on `need`'s thread right now.
///
/// # Errors
/// Returns a [`VivariumError`] when the thread cannot be read or a candidate
/// reply's date does not parse.
pub(crate) fn checkpoint(ctx: &Ctx, need: &str) -> Result<String, VivariumError> {
    let Some(mailspace) = open_mailspace(ctx) else {
        return Ok(EMPTY.to_owned());
    };
    encode(&eligible(&mailspace, ctx.cfg.reply_from.as_str(), need)?)
}

/// The eligible need-reply message IDs on `need`'s thread, oldest first.
///
/// # Errors
/// Returns a [`VivariumError`] when the thread cannot be read or a candidate
/// reply's date does not parse.
pub(crate) fn eligible(
    mailspace: &Mailspace,
    pump: &str,
    need: &str,
) -> Result<Vec<String>, VivariumError> {
    let thread = mailspace.thread(need, false, usize::MAX, usize::MAX)?;
    replies(mailspace, pump, need, &thread)
}

/// The eligible messages of `thread`: not the root, not a lifecycle event,
/// not the pump's own mail, not a pump record or action letter.
fn replies(
    mailspace: &Mailspace,
    pump: &str,
    need: &str,
    thread: &[MailspaceThreadMessage],
) -> Result<Vec<String>, VivariumError> {
    let mut ids = Vec::new();
    for (index, message) in thread.iter().enumerate() {
        if is_root(need, index, message)
            || message.event.is_some()
            || is_pump_authored(mailspace, pump, message)
            || is_pump_letter(message)
        {
            continue;
        }
        visible_date(&message.date)?;
        ids.push(message.message_id.clone());
    }
    Ok(ids)
}

/// The need's own seed letter, or an anchor letter with no handle of its own.
fn is_root(need: &str, index: usize, message: &MailspaceThreadMessage) -> bool {
    message.handle == need || (message.handle.is_empty() && index == 0)
}

/// Whether the pump identity authored `message`.
fn is_pump_authored(mailspace: &Mailspace, pump: &str, message: &MailspaceThreadMessage) -> bool {
    matches!(mailspace.classify_recipient(&message.from), Ok(Recipient::Local(id)) if id == pump)
}

/// A pump record or a posted operator action letter, never a reply.
fn is_pump_letter(message: &MailspaceThreadMessage) -> bool {
    message.kind.as_deref() == Some("pump")
        || (message.subject.starts_with("pump ") && message.subject.ends_with(" action"))
}

/// A thread date must parse: a corrupt date fails the capture visibly instead
/// of quietly narrowing the recorded input.
fn visible_date(date: &str) -> Result<(), VivariumError> {
    DateTime::parse_from_rfc3339(date)
        .map(|_| ())
        .map_err(|error| VivariumError::Parse(format!("thread date {date:?}: {error}")))
}

/// The checkpoint of `ids`: a JSON array, oldest first.
fn encode(ids: &[String]) -> Result<String, VivariumError> {
    to_string(ids)
        .map_err(|error| VivariumError::Other(format!("recovery inputs: encode ids: {error}")))
}

#[cfg(test)]
#[path = "recovery_inputs_test.rs"]
mod tests;
