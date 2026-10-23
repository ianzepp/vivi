//! Pool dispatch records on a task.
//!
//! A dispatch and its collected result are recorded as typed events on the
//! task (`pool_dispatched`, `pool_collected`), each paired with a reply note in
//! the task's thread. The event note carries the machine-readable record; the
//! reply is the human-readable receipt. The task itself never moves.

use super::Mailspace;
use super::thread::{MailspaceThreadMessage, text_body};
use crate::error::VivariumError;
use crate::letter::records::NoteWord;
use crate::storage::{MailspaceEventInput, MailspaceNoteWithReply, StoredMessageView};

/// A task as the pool reads it.
#[derive(Debug, Clone)]
pub struct PoolTask {
    pub handle: String,
    pub subject: String,
    /// The task body: the brief handed to the remote worker.
    pub body: String,
    /// The canonical name of the local role the task copy is held under or
    /// addressed to, when that role exists.
    pub addressee: Option<String>,
}

/// One pool event on a task, oldest first in listings.
#[derive(Debug, Clone)]
pub struct PoolEventRow {
    pub handle: String,
    pub event_type: String,
    pub note: String,
}

/// First line of a pump attempt-context reply on a slice task.
pub const ATTEMPT_MARKER: &str = "pump-attempt:";

/// First line of a pump brief reply on a preset task.
pub const BRIEF_MARKER: &str = "pump-brief:";

/// The first line of a pump-marked reply: `<marker> t=<unix micros> <rest>`.
///
/// Thread order has one-second precision, so same-second replies need their
/// own order. The stamp is the clock in microseconds, pushed one past the last
/// stamp this process issued, so it strictly increases.
#[must_use]
pub fn marked_line(marker: &str, rest: &str) -> String {
    let stamp = super::pump_note::next_stamp();
    format!("{marker} t={stamp} {rest}").trim_end().to_owned()
}

/// The text after `marker` on a marked line, without the `t=` stamp. `None`
/// when the line does not start with the marker.
#[must_use]
pub fn marked_payload<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix(marker)?.trim_start();
    Some(match split_stamp(rest) {
        Some((_, after)) => after,
        None => rest,
    })
}

/// A leading `t=<digits>` token and the text after it.
fn split_stamp(text: &str) -> Option<(i64, &str)> {
    let (token, after) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let stamp = match token.strip_prefix("t=")?.parse() {
        Ok(stamp) => stamp,
        Err(_) => return None,
    };
    Some((stamp, after.trim_start()))
}

/// The order stamp of a marked message: a note's `pump.t`, else the stamp on
/// the first line, when it has one.
fn stamp_of(body: &str) -> Option<i64> {
    if let Some(note) = super::pump_note::note_of(body) {
        return Some(note.pump.t);
    }
    // TRANSITIONAL (W3-B3): the old first-line stamp, deleted by W3B3-15.
    let first = body.trim_start().lines().next()?;
    let (_, rest) = first.split_once(char::is_whitespace)?;
    split_stamp(rest.trim_start()).map(|(stamp, _)| stamp)
}

/// Sort marked replies oldest first by `(t, Date, message_id)`. A reply with
/// no stamp sorts by `Date` and then `message_id`, after any stamped reply
/// of the same second.
pub fn order_marked(replies: &mut [MailspaceThreadMessage]) {
    replies.sort_by_cached_key(|message| {
        let stamp = stamp_of(&message.body);
        let second = stamp.map_or_else(|| date_seconds(&message.date), |t| t.div_euclid(1_000_000));
        (
            second,
            stamp.is_none(),
            stamp.unwrap_or(0),
            message.date.clone(),
            message.message_id.clone(),
        )
    });
}

/// Unix seconds of a thread `Date`, 0 when it does not parse.
fn date_seconds(date: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(date)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(date))
        .map_or(0, |parsed| parsed.timestamp())
}

/// The marked replies found on one task's thread.
#[derive(Default)]
struct ThreadReplies {
    briefs: Vec<String>,
    attempts: Vec<String>,
}

/// Heading under which a task's earlier attempts are appended to its body.
const ATTEMPTS_HEADING: &str = "## Earlier attempts";

/// Most bytes of attempt replies appended to a task body.
const ATTEMPTS_CAP: usize = 8 * 1024;

/// Line that opens the attempt text when older replies were cut.
const ATTEMPTS_CUT: &str = "[older attempts cut]";

/// The task letter with `brief` as its prose: the task's own front matter
/// stays, the brief's marker line is dropped. A task body that does not parse
/// as a task letter is replaced by the brief unchanged, so the dispatcher's
/// refusal names the task.
fn briefed_body(task_body: &str, brief: &str) -> String {
    let prose = match brief.split_once('\n') {
        Some((first, rest)) if first.starts_with(BRIEF_MARKER) => rest.trim_start_matches('\n'),
        _ => brief,
    };
    match crate::letter::read::task_of("", task_body) {
        Ok(envelope) => match crate::letter::render(&envelope.front, prose) {
            Ok(rendered) => rendered,
            Err(_) => brief.to_owned(),
        },
        Err(_) => brief.to_owned(),
    }
}

/// Event types the pool writes.
pub const POOL_EVENT_TYPES: [&str; 2] = ["pool_dispatched", "pool_collected"];

impl Mailspace {
    /// Read an open, unabsorbed task by handle or message id.
    ///
    /// # Errors
    /// Returns an error if the token does not resolve, the item is not a
    /// task, it is already done or absorbed, or its body cannot be read.
    pub fn pool_task(&self, token: &str) -> Result<PoolTask, VivariumError> {
        let storage = self.storage()?;
        let id = storage.resolve_message_token(token)?;
        let view = storage
            .message_by_id(&id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {token}")))?;
        if self.source_kind(&storage, &view)? != "task" {
            return Err(VivariumError::Message(format!("{token} is not a task")));
        }
        if view.local_role == "done" || view.absorbed_at.is_some() {
            return Err(VivariumError::Message(format!(
                "{token} is done or absorbed; dispatch needs an open task"
            )));
        }
        let data = storage.read_message(&id)?;
        let handle = storage.display_handle(&id)?;
        let thread = self.marked_thread_replies(&handle, &id)?;
        // A preset-task job carries its brief as a reply; the newest brief
        // becomes the prose of the task letter, which keeps its own front.
        let task_body = text_body(&data);
        let body = match thread.briefs.into_iter().next_back() {
            Some(brief) => briefed_body(&task_body, &brief),
            None => task_body,
        };
        let body = with_attempts(body, thread.attempts);
        // The copy is held under the recipient's account; a recipient that
        // is not a known local role leaves no addressee.
        let addressee = match self.resolve_identity(&view.account) {
            Ok(name) => Some(name),
            Err(_) => None,
        };
        Ok(PoolTask {
            handle: storage.display_handle(&id)?,
            subject: view.subject,
            body,
            addressee,
        })
    }

    /// The `pump-brief:` replies and the attempt notes on a task's thread,
    /// each oldest first.
    fn marked_thread_replies(
        &self,
        handle: &str,
        task_id: &str,
    ) -> Result<ThreadReplies, VivariumError> {
        let mut thread = self.thread(handle, false, usize::MAX, usize::MAX)?;
        order_marked(&mut thread);
        let mut found = ThreadReplies::default();
        for message in thread {
            if message.event.is_some() || message.message_id == task_id {
                continue;
            }
            let body = message.body.trim();
            if let Some(note) = super::pump_note::note_of(body) {
                if note.pump.note == NoteWord::Attempt {
                    found.attempts.push(note.prose.trim().to_owned());
                }
            } else if body.starts_with(BRIEF_MARKER) {
                found.briefs.push(body.to_owned());
            }
        }
        Ok(found)
    }

    /// The display handle of any item, for matching against pool events.
    ///
    /// # Errors
    /// Returns an error if the token does not resolve.
    pub fn pool_handle(&self, token: &str) -> Result<String, VivariumError> {
        let storage = self.storage()?;
        let id = storage.resolve_message_token(token)?;
        storage.display_handle(&id)
    }

    /// Every pool event in the mailspace, oldest first, with display handles.
    ///
    /// # Errors
    /// Returns an error if the event log cannot be read.
    pub fn pool_events(&self) -> Result<Vec<PoolEventRow>, VivariumError> {
        let storage = self.storage()?;
        let events = storage.list_mailspace_events_of_types(&POOL_EVENT_TYPES)?;
        events
            .into_iter()
            .map(|event| {
                Ok(PoolEventRow {
                    handle: storage.display_handle(&event.message_id)?,
                    event_type: event.event_type,
                    note: event.note.unwrap_or_default(),
                })
            })
            .collect()
    }

    /// Record a pool event on a task together with a reply note carrying
    /// `text`. Both land in one transaction.
    ///
    /// # Errors
    /// Returns an error if the task cannot be resolved or is absorbed, or the
    /// write fails.
    pub fn record_pool_note(
        &self,
        task: &str,
        event_type: &str,
        record: &str,
        text: &str,
    ) -> Result<(), VivariumError> {
        let storage = self.storage()?;
        let id = storage.resolve_message_token(task)?;
        let view = storage
            .message_by_id(&id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {task}")))?;
        let identity = self
            .identity_owning_account(&view.account)
            .unwrap_or_else(|| view.account.clone());
        let reply = self.note_reply(&storage, &identity, &view, text, &[])?;
        let event = pool_event(&view, &identity, event_type, record);
        storage.note_message_with_reply(&MailspaceNoteWithReply {
            message_id: &id,
            event: &event,
            reply_requests: &reply.requests,
            reply_data: &reply.data,
            parent_content_id: &view.content_id,
        })?;
        Ok(())
    }
}

/// `body` with the attempt replies appended under a heading, newest last. A
/// task with none is unchanged. Past the cap the oldest text is dropped and
/// a cut marker opens the section.
fn with_attempts(body: String, replies: Vec<String>) -> String {
    if replies.is_empty() {
        return body;
    }
    let joined = replies.join("\n\n");
    let section = if joined.len() > ATTEMPTS_CAP {
        let mut start = joined.len() - ATTEMPTS_CAP;
        while !joined.is_char_boundary(start) {
            start += 1;
        }
        format!("{ATTEMPTS_CUT}\n{}", &joined[start..])
    } else {
        joined
    };
    format!("{}\n\n{ATTEMPTS_HEADING}\n\n{section}\n", body.trim_end())
}

fn pool_event(
    view: &StoredMessageView,
    identity: &str,
    event_type: &str,
    record: &str,
) -> MailspaceEventInput {
    MailspaceEventInput {
        command: format!("pool {}", event_type.trim_start_matches("pool_")),
        event_type: event_type.into(),
        actor_identity: Some(identity.into()),
        account: view.account.clone(),
        message_id: view.message_id.clone(),
        content_id: view.content_id.clone(),
        from_role: Some(view.local_role.clone()),
        to_role: Some(view.local_role.clone()),
        from_identity: None,
        to_identity: Some(view.account.clone()),
        subject: view.subject.clone(),
        note: Some(record.into()),
    }
}

#[cfg(test)]
#[path = "pool_record_test.rs"]
mod tests;
