//! Recovery inputs: the need-reply checkpoint of one execution.
//!
//! At execution input capture (a handler job's enqueue) the pump snapshots
//! the message IDs of the eligible need replies on the need's thread: exactly
//! the letters that are part of the work's input. A reply that arrives after
//! the capture is not in the snapshot, so recovery compares message IDs
//! instead of trusting the one-second resolution a thread date has. The
//! exclusions are shared with the thread-change reader: the thread's root
//! letter, lifecycle event letters, the pump's own mail, pump record letters
//! and posted action letters are never replies.
//!
//! An eligible letter whose date does not parse stays a visible error rather
//! than being silently skipped: a malformed date would make the thread's
//! order unreliable for every reader that still uses it.

use chrono::DateTime;

use crate::error::VivariumError;
use crate::mailspace::{Mailspace, MailspaceThreadMessage, Recipient};

/// The eligible need replies of `need`'s thread as checkpoint text: one
/// comma-joined list of message IDs, oldest first, empty when the thread
/// holds no eligible reply.
///
/// # Errors
/// Returns [`VivariumError`] when the thread cannot be read or an eligible
/// letter's date does not parse.
pub fn capture(mailspace: &Mailspace, pump: &str, need: &str) -> Result<String, VivariumError> {
    let thread = mailspace.thread(need, false, usize::MAX, usize::MAX)?;
    checkpoint(mailspace, pump, &thread)
}

/// The comma-joined message IDs of every eligible reply on `thread`, oldest
/// first. Every eligible letter's date is validated even though the IDs, not
/// the dates, decide membership.
///
/// # Errors
/// Returns [`VivariumError`] when an eligible letter's date does not parse.
pub fn checkpoint(
    mailspace: &Mailspace,
    pump: &str,
    thread: &[MailspaceThreadMessage],
) -> Result<String, VivariumError> {
    let mut ids = Vec::new();
    for message in thread {
        if !is_reply(mailspace, pump, message) {
            continue;
        }
        validated_date(&message.date)?;
        ids.push(message.message_id.clone());
    }
    Ok(ids.join(","))
}

/// Whether `message` is an eligible need reply at execution input capture:
/// a reply (not the thread's root letter), not a lifecycle event letter,
/// not the pump's own mail, not a pump record letter and not a posted
/// action letter.
#[must_use]
pub fn is_reply(mailspace: &Mailspace, pump: &str, message: &MailspaceThreadMessage) -> bool {
    !is_root(message)
        && message.event.is_none()
        && !is_pump(mailspace, pump, message)
        && !is_pump_letter(message)
}

/// The thread's root letter: the anchor itself, which arrived as no reply.
fn is_root(message: &MailspaceThreadMessage) -> bool {
    message.parent_content_id.is_none()
}

/// Mail the pump identity sent.
fn is_pump(mailspace: &Mailspace, pump: &str, message: &MailspaceThreadMessage) -> bool {
    matches!(
        mailspace.classify_recipient(&message.from),
        Ok(Recipient::Local(id)) if id == pump
    )
}

/// A pump record or a posted action letter, never a reply on the thread.
fn is_pump_letter(message: &MailspaceThreadMessage) -> bool {
    message.kind.as_deref() == Some("pump")
        || (message.subject.starts_with("pump ") && message.subject.ends_with(" action"))
}

/// The epoch seconds of an RFC 3339 thread date: a visible error when it
/// does not parse.
fn validated_date(date: &str) -> Result<i64, VivariumError> {
    DateTime::parse_from_rfc3339(date)
        .map(|parsed| parsed.timestamp())
        .map_err(|error| VivariumError::Parse(format!("thread date {date:?}: {error}")))
}

#[cfg(test)]
#[path = "recovery_inputs_test.rs"]
mod tests;
