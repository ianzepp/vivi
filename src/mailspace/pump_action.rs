//! Operator actions on a pump record.
//!
//! Requeue, withdraw, replan, answer and unlock are things an operator asks of one dispatch,
//! so each is a structured reply on the need's open pump record. The letter is a `mail`
//! letter whose body opens with `ActionFront` front matter and whose prose is the reason:
//!
//! | key | value |
//! | --- | --- |
//! | `action` | `requeue`, `withdraw`, `replan`, `answer` or `unlock` (`recover` is retired: it cannot be posted) |
//! | `need` | the need the record covers |
//! | `unit` | requeue: the unit to requeue |
//! | `attempts` | requeue: set the attempt counter to N |
//! | `fresh` | requeue: `true` resets the attempt counter |
//! | `refund` | requeue: `true` refunds transient failures |
//! | `force` | replan: `true` replans past live work |
//! | `step` | answer: the flow step task being answered |
//! | `word` | answer: the answer word; unlock: the entry to restart at |
//!
//! The record the action was posted on is the letter's reply target and the note of its
//! `pump action` / `posted` event; it is never read from the letter.
//!
//! Posting writes the letter and one `pump action` / `posted` event in one
//! store transaction. The pump finds unapplied actions from those events
//! alone, with no thread reads, and applies each exactly once (the pump keeps
//! the applied letters' ids with its state). An action posted on a record
//! that is closed is never applied: the need has no open dispatch to act on.
//! An action letter that does not parse (an old-format letter with no front
//! matter, or invalid front matter) is never applied and is not returned by
//! `pump_pending_actions`; `pump_unreadable_actions` lists it so the pump can
//! raise one alarm per letter.
//!
//! Everything here is library code and prints nothing, so a CLI or a hosted
//! server can post the same actions.

use std::collections::BTreeSet;

use serde::Serialize;

use super::pump_record::{PumpState, in_write_batch};
use super::{Mailspace, SendRequest};
use crate::error::VivariumError;
use crate::letter::records::{ActionFront, ActionWord};
use crate::letter::{FENCE, LetterError, parse_letter};
use crate::storage::{MailspaceEvent, MailspaceEventInput, Storage, StoredMessageView};

/// The event command and type every posted action logs.
const EVENT_COMMAND: &str = "pump action";
const EVENT_TYPE: &str = "posted";

/// What an operator asks of a dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PumpActionKind {
    /// Put one stuck, quarantined, splitting or withdrawn unit back in line.
    Requeue,
    /// Withdraw the need's unfinished units and close the dispatch.
    Withdraw,
    /// Throw the plan away and plan the need again on a fresh record.
    Replan,
    /// Retired: it cannot be posted.
    Recover,
    /// Answer a flow step by hand; applied by the flow engine, not the old pump.
    Answer,
    /// Release a locked flow run; applied by the flow engine, not the old pump.
    Unlock,
}

impl PumpActionKind {
    /// The word the CLI prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requeue => "requeue",
            Self::Withdraw => "withdraw",
            Self::Replan => "replan",
            Self::Recover => "recover",
            Self::Answer => "answer",
            Self::Unlock => "unlock",
        }
    }

    /// The front matter word for this kind; `Recover` has none.
    fn word(self) -> Option<ActionWord> {
        match self {
            Self::Requeue => Some(ActionWord::Requeue),
            Self::Withdraw => Some(ActionWord::Withdraw),
            Self::Replan => Some(ActionWord::Replan),
            Self::Answer => Some(ActionWord::Answer),
            Self::Unlock => Some(ActionWord::Unlock),
            Self::Recover => None,
        }
    }

    fn of_word(word: ActionWord) -> Self {
        match word {
            ActionWord::Requeue => Self::Requeue,
            ActionWord::Withdraw => Self::Withdraw,
            ActionWord::Replan => Self::Replan,
            ActionWord::Answer => Self::Answer,
            ActionWord::Unlock => Self::Unlock,
        }
    }
}

/// One action to post: its kind and the parameters that kind reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PumpActionRequest {
    pub kind: PumpActionKind,
    /// Requeue: the unit.
    pub unit: Option<String>,
    /// Requeue: set the attempt counter to this value.
    pub attempts: Option<u32>,
    /// Requeue: reset the attempt counter to zero.
    pub fresh: bool,
    /// Requeue: refund the attempts that failed for a transient reason.
    pub refund: bool,
    /// Replan: replan even while units are live or a planner runs.
    pub force: bool,
    /// Answer: the flow step task.
    pub step: Option<String>,
    /// Answer: the answer word. Unlock: the entry to restart at.
    pub word: Option<String>,
    /// Why; the letter's body.
    pub reason: String,
}

impl PumpActionRequest {
    fn new(kind: PumpActionKind, reason: &str) -> Self {
        Self {
            kind,
            unit: None,
            attempts: None,
            fresh: false,
            refund: false,
            force: false,
            step: None,
            word: None,
            reason: reason.trim().to_string(),
        }
    }

    /// Requeue `unit`; `attempts`, `fresh` and `refund` move its attempt
    /// counter (at most one is meaningful; the pump applies them in that
    /// order).
    #[must_use]
    pub fn requeue(
        unit: &str,
        attempts: Option<u32>,
        fresh: bool,
        refund: bool,
        reason: &str,
    ) -> Self {
        Self {
            unit: Some(unit.to_string()),
            attempts,
            fresh,
            refund,
            ..Self::new(PumpActionKind::Requeue, reason)
        }
    }

    /// Withdraw the need.
    #[must_use]
    pub fn withdraw(reason: &str) -> Self {
        Self::new(PumpActionKind::Withdraw, reason)
    }

    /// Replan the need.
    #[must_use]
    pub fn replan(force: bool) -> Self {
        Self {
            force,
            ..Self::new(PumpActionKind::Replan, "operator replan")
        }
    }

    /// Answer the flow step task `step` with `word`; `note` is the body.
    #[must_use]
    pub fn answer(step: &str, word: &str, note: &str) -> Self {
        Self {
            step: Some(step.to_string()),
            word: Some(word.trim().to_string()),
            ..Self::new(PumpActionKind::Answer, note)
        }
    }

    /// Unlock a locked flow run; `entry` is the flow entry to restart at
    /// (the flow's first entry when `None`) and `note` is the body.
    #[must_use]
    pub fn unlock(entry: Option<&str>, note: &str) -> Self {
        Self {
            word: entry.map(|entry| entry.trim().to_string()),
            ..Self::new(PumpActionKind::Unlock, note)
        }
    }

    /// The front matter this request writes; `None` for a retired kind.
    fn front(&self, need: &str) -> Option<ActionFront> {
        let text = |value: &Option<String>| value.clone().filter(|text| !text.trim().is_empty());
        Some(ActionFront {
            action: self.kind.word()?,
            need: need.to_string(),
            unit: text(&self.unit),
            attempts: self.attempts,
            fresh: self.fresh,
            refund: self.refund,
            force: self.force,
            step: text(&self.step),
            word: text(&self.word),
        })
    }

    /// The request a letter's front matter and prose describe.
    fn from_front(front: ActionFront, prose: &str) -> Self {
        Self {
            kind: PumpActionKind::of_word(front.action),
            unit: front.unit,
            attempts: front.attempts,
            fresh: front.fresh,
            refund: front.refund,
            force: front.force,
            step: front.step,
            word: front.word,
            reason: prose.trim().to_string(),
        }
    }
}

/// An action that was posted on a pump record.
#[derive(Debug, Clone, Serialize)]
pub struct PumpAction {
    /// The action letter's content id: stable across its copies, and what the
    /// pump records as applied.
    pub id: String,
    /// The pump record it was posted on.
    pub record: String,
    /// The need that record covers.
    pub need: String,
    /// When it was posted.
    pub date: String,
    pub request: PumpActionRequest,
}

/// An action letter that was posted on an open record but does not parse.
#[derive(Debug, Clone, Serialize)]
pub struct UnreadableAction {
    /// The action letter's content id, as `PumpAction::id`.
    pub id: String,
    /// The letter's display handle.
    pub handle: String,
    /// The pump record it was posted on.
    pub record: String,
    /// Why it does not parse; names the handle.
    pub error: String,
}

/// One posted action letter: readable or not.
enum Posted {
    Action(PumpAction),
    Unreadable(UnreadableAction),
}

/// What one action letter is made of.
struct ActionLetter<'a> {
    from: &'a str,
    to: &'a str,
    need: &'a str,
    record: &'a str,
    request: &'a PumpActionRequest,
}

impl Mailspace {
    /// Post an operator action on `need`'s open pump record: a reply on the
    /// record, from `from` (default `[pump] from`) to the pump identity.
    ///
    /// The open-record check and the post run in one store write transaction,
    /// so an action can never land on a record that a concurrent close has
    /// just ended.
    ///
    /// # Errors
    /// Returns an error if the need is unknown or not a need, if it has no
    /// open pump record (the error says how to open one), if a requeue names
    /// no unit, if an identity does not resolve, or if the send fails. A
    /// failed post leaves nothing behind.
    pub fn pump_post_action(
        &self,
        from: Option<&str>,
        need: &str,
        request: &PumpActionRequest,
    ) -> Result<PumpAction, VivariumError> {
        let storage = self.storage()?;
        in_write_batch(&storage, || {
            self.post_checked(&storage, from, need, request)
        })
    }

    /// Post an operator answer to the flow step task `step_task`, on the
    /// open pump record of the need the step belongs to.
    ///
    /// The need is read from the step task's own `[step]` table (a pump step
    /// or run sentinel); a task without one is refused.
    /// The note (may be empty) is the letter's body.
    ///
    /// # Errors
    /// Returns an error if the task does not resolve, is not a pump step or
    /// run sentinel, its need has no open pump record, the word is blank, or
    /// the send fails.
    pub fn pump_post_answer(
        &self,
        from: Option<&str>,
        step_task: &str,
        word: &str,
        note: &str,
    ) -> Result<PumpAction, VivariumError> {
        if word.trim().is_empty() {
            return Err(VivariumError::Message(
                "an answer action names the word to answer".into(),
            ));
        }
        let storage = self.storage()?;
        in_write_batch(&storage, || {
            let need = step_need(&storage, step_task)?;
            let request = PumpActionRequest::answer(step_task, word, note);
            self.post_checked(&storage, from, &need, &request)
        })
    }

    fn post_checked(
        &self,
        storage: &Storage,
        from: Option<&str>,
        need: &str,
        request: &PumpActionRequest,
    ) -> Result<PumpAction, VivariumError> {
        if request.kind == PumpActionKind::Recover {
            return Err(VivariumError::Message(
                "recover is retired: there is no recover action letter".into(),
            ));
        }
        if request.kind == PumpActionKind::Requeue && request.unit.is_none() {
            return Err(VivariumError::Message(
                "a requeue action names the unit to requeue".into(),
            ));
        }
        let (default_from, to) = self.pump_names();
        let (need_handle, _) = self.resolve_need(storage, need)?;
        let record = self.open_record_of(storage, &need_handle)?.ok_or_else(|| {
            VivariumError::Message(format!(
                "need {need_handle} has no open pump record, so there is no dispatch to {}; \
                     open one with `vivi pump send --need {need_handle}`",
                request.kind.as_str()
            ))
        })?;
        let from = from.map_or(default_from, str::to_string);
        let letter = ActionLetter {
            from: &from,
            to: &to,
            need: &need_handle,
            record: &record.handle,
            request,
        };
        let view = self.send_letter(storage, &letter)?;
        Ok(PumpAction {
            id: view.content_id,
            record: record.handle,
            need: need_handle,
            date: view.date,
            request: request.clone(),
        })
    }

    /// Send one action letter as a reply on its record and log the `posted`
    /// event the pump finds it by; returns the sent copy's row.
    fn send_letter(
        &self,
        storage: &Storage,
        letter: &ActionLetter<'_>,
    ) -> Result<StoredMessageView, VivariumError> {
        let request = letter.request;
        let front = request.front(letter.need).ok_or_else(|| {
            VivariumError::Message("recover is retired: there is no recover action letter".into())
        })?;
        let prose = if request.reason.is_empty() {
            format!("{} {}", request.kind.as_str(), letter.need)
        } else {
            request.reason.clone()
        };
        let body = crate::letter::render(&front, &prose)
            .map_err(|error| VivariumError::Message(error.to_string()))?;
        let result = self.send(SendRequest {
            from: letter.from.to_string(),
            to: vec![letter.to.to_string()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("pump {}: {} action", letter.need, request.kind.as_str()),
            body,
            role: "inbox".into(),
            kind: Some("mail".into()),
            reply_to: Some(letter.record.to_string()),
        })?;
        let message_id = storage.resolve_message_token(&result.sent)?;
        let view = storage.message_by_id(&message_id)?.ok_or_else(|| {
            VivariumError::Message("the posted action is missing from the store".into())
        })?;
        storage.append_mailspace_event(&MailspaceEventInput {
            command: EVENT_COMMAND.into(),
            event_type: EVENT_TYPE.into(),
            actor_identity: Some(letter.from.to_string()),
            account: letter.from.to_string(),
            message_id: view.message_id.clone(),
            content_id: view.content_id.clone(),
            from_role: None,
            to_role: Some("inbox".into()),
            from_identity: Some(letter.from.to_string()),
            to_identity: None,
            subject: view.subject.clone(),
            note: Some(letter.record.to_string()),
        })?;
        Ok(view)
    }

    /// The actions posted on a record that is still open and whose id is not
    /// in `applied`, oldest first. Closed records are skipped: an action on a
    /// finished dispatch has nothing left to act on. A posted letter that does
    /// not parse as an action is not returned here and is never applied; see
    /// [`Mailspace::pump_unreadable_actions`].
    ///
    /// # Errors
    /// Returns an error if the event log or a letter cannot be read.
    pub fn pump_pending_actions(
        &self,
        applied: &BTreeSet<String>,
    ) -> Result<Vec<PumpAction>, VivariumError> {
        let mut pending = Vec::new();
        for posted in self.posted_actions(applied)? {
            if let Posted::Action(action) = posted {
                pending.push(action);
            }
        }
        Ok(pending)
    }

    /// The action letters posted on a record that is still open, not in
    /// `applied`, that do not parse as an action, oldest first. They are never
    /// applied; the pump raises one alarm per letter.
    ///
    /// # Errors
    /// Returns an error if the event log or a letter cannot be read.
    pub fn pump_unreadable_actions(
        &self,
        applied: &BTreeSet<String>,
    ) -> Result<Vec<UnreadableAction>, VivariumError> {
        let mut unreadable = Vec::new();
        for posted in self.posted_actions(applied)? {
            if let Posted::Unreadable(letter) = posted {
                unreadable.push(letter);
            }
        }
        Ok(unreadable)
    }

    /// Every posted action letter on an open record that is not applied.
    fn posted_actions(&self, applied: &BTreeSet<String>) -> Result<Vec<Posted>, VivariumError> {
        let storage = self.storage()?;
        let events = storage.list_mailspace_events_for_command(EVENT_COMMAND, EVENT_TYPE)?;
        if events
            .iter()
            .all(|event| applied.contains(&event.content_id))
        {
            return Ok(Vec::new());
        }
        let open: BTreeSet<String> = self
            .pump_records(&storage, Some(PumpState::Open))?
            .into_iter()
            .map(|record| record.handle)
            .collect();
        let mut posted = Vec::new();
        for event in &events {
            if applied.contains(&event.content_id) {
                continue;
            }
            if let Some(found) = self.action_of(&storage, event, &open)? {
                posted.push(found);
            }
        }
        Ok(posted)
    }

    /// One posted action, when its record is open: the action when its letter
    /// parses, the unreadable letter when it does not.
    fn action_of(
        &self,
        storage: &Storage,
        event: &MailspaceEvent,
        open: &BTreeSet<String>,
    ) -> Result<Option<Posted>, VivariumError> {
        let Some(record) = event
            .note
            .as_deref()
            .filter(|record| open.contains(*record))
        else {
            return Ok(None);
        };
        let data = storage.read_message(&event.message_id)?;
        let body = crate::extract::extract_text(&data)?;
        let handle = storage.display_handle(&event.message_id)?;
        match read_front(&handle, &body) {
            Ok(envelope) => {
                let need = envelope.front.need.clone();
                Ok(Some(Posted::Action(PumpAction {
                    id: event.content_id.clone(),
                    record: record.to_string(),
                    need,
                    date: event.occurred_at.clone(),
                    request: PumpActionRequest::from_front(envelope.front, &envelope.body),
                })))
            }
            Err(error) => Ok(Some(Posted::Unreadable(UnreadableAction {
                id: event.content_id.clone(),
                handle,
                record: record.to_string(),
                error: error.to_string(),
            }))),
        }
    }
}

/// An action letter's front matter. A body with no front matter is a legacy
/// letter; invalid front matter names the letter in its error.
fn read_front(
    handle: &str,
    body: &str,
) -> Result<crate::letter::Envelope<ActionFront>, LetterError> {
    let opens = body
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.trim() == FENCE);
    if !opens {
        return Err(LetterError::Legacy {
            handle: handle.to_string(),
            kind: "mail".to_string(),
        });
    }
    parse_letter(body)
        .map_err(|error| LetterError::Invalid(format!("action letter {handle}: {error}")))
}

/// The need a pump step task or run sentinel belongs to, read from its
/// `[step]` table.
fn step_need(storage: &Storage, task: &str) -> Result<String, VivariumError> {
    let message_id = storage
        .resolve_message_token(task)
        .map_err(|error| VivariumError::Message(format!("unknown task '{task}': {error}")))?;
    let body = crate::extract::extract_text(&storage.read_message(&message_id)?)?;
    crate::letter::read::step_of(&body)
        .map(|step| step.need)
        .ok_or_else(|| {
            VivariumError::Message(format!(
                "{task} is not a pump step task: it has no [step] table"
            ))
        })
}

#[cfg(test)]
#[path = "pump_action_test.rs"]
mod tests;
