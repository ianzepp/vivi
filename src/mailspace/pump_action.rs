//! Operator actions on a pump record.
//!
//! Requeue, withdraw, replan, answer and unlock are things an operator asks of one dispatch,
//! so each is a structured reply on the need's open pump record. The letter
//! carries its action in headers and its free text (the reason) in the body:
//!
//! | header | value |
//! | --- | --- |
//! | `X-Vivi-Pump-Action` | `requeue`, `withdraw`, `replan`, `answer` or `unlock` (`recover` is retired: still read, changes nothing) |
//! | `X-Vivi-Pump-Record` | the record the action was posted on |
//! | `X-Vivi-Need` | the need that record covers |
//! | `X-Vivi-Pump-Unit` | requeue: the unit to requeue |
//! | `X-Vivi-Pump-Attempts` | requeue: set the attempt counter to N |
//! | `X-Vivi-Pump-Fresh` | requeue: `1` resets the attempt counter |
//! | `X-Vivi-Pump-Refund` | requeue: `1` refunds transient failures |
//! | `X-Vivi-Pump-Force` | replan: `1` replans past live work |
//! | `X-Vivi-Pump-Target` | recover (retired): carried, never read |
//! | `X-Vivi-Pump-Step` | answer: the flow step task being answered |
//! | `X-Vivi-Pump-Word` | answer: the answer word; the note is the body; unlock: the entry to restart at |
//!
//! Posting writes the letter and one `pump action` / `posted` event in one
//! store transaction. The pump finds unapplied actions from those events
//! alone, with no thread reads, and applies each exactly once (the pump keeps
//! the applied letters' ids with its state). An action posted on a record
//! that is closed is never applied: the need has no open dispatch to act on.
//!
//! Everything here is library code and prints nothing, so a CLI or a hosted
//! server can post the same actions.

use std::collections::BTreeSet;

use serde::Serialize;

use super::pump_record::{PumpState, header_value, in_write_batch};
use super::{Mailspace, SendRequest};
use crate::error::VivariumError;
use crate::storage::{MailspaceEvent, MailspaceEventInput, Storage, StoredMessageView};

/// The header naming the action.
pub const PUMP_ACTION_HEADER: &str = "X-Vivi-Pump-Action";
/// The header naming the record the action was posted on.
pub const PUMP_RECORD_HEADER: &str = "X-Vivi-Pump-Record";
const UNIT_HEADER: &str = "X-Vivi-Pump-Unit";
const ATTEMPTS_HEADER: &str = "X-Vivi-Pump-Attempts";
const FRESH_HEADER: &str = "X-Vivi-Pump-Fresh";
const REFUND_HEADER: &str = "X-Vivi-Pump-Refund";
const FORCE_HEADER: &str = "X-Vivi-Pump-Force";
const TARGET_HEADER: &str = "X-Vivi-Pump-Target";
const STEP_HEADER: &str = "X-Vivi-Pump-Step";
const WORD_HEADER: &str = "X-Vivi-Pump-Word";

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
    /// Retired: a letter of this kind is read and settled, and changes nothing.
    Recover,
    /// Answer a flow step by hand; applied by the flow engine, not the old pump.
    Answer,
    /// Release a locked flow run; applied by the flow engine, not the old pump.
    Unlock,
}

impl PumpActionKind {
    /// The header value and the word the CLI prints.
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

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "requeue" => Some(Self::Requeue),
            "withdraw" => Some(Self::Withdraw),
            "replan" => Some(Self::Replan),
            "recover" => Some(Self::Recover),
            "answer" => Some(Self::Answer),
            "unlock" => Some(Self::Unlock),
            _ => None,
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
    /// Recover (retired): carried in the header, never read.
    pub target: Option<String>,
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
            target: None,
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

    /// The headers the letter carries, besides the record and need headers.
    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![(PUMP_ACTION_HEADER.to_string(), self.kind.as_str().into())];
        if let Some(unit) = &self.unit {
            headers.push((UNIT_HEADER.into(), unit.clone()));
        }
        if let Some(attempts) = self.attempts {
            headers.push((ATTEMPTS_HEADER.into(), attempts.to_string()));
        }
        if let Some(target) = &self.target {
            headers.push((TARGET_HEADER.into(), target.clone()));
        }
        if let Some(step) = &self.step {
            headers.push((STEP_HEADER.into(), step.clone()));
        }
        if let Some(word) = &self.word {
            headers.push((WORD_HEADER.into(), word.clone()));
        }
        for (name, set) in [
            (FRESH_HEADER, self.fresh),
            (REFUND_HEADER, self.refund),
            (FORCE_HEADER, self.force),
        ] {
            if set {
                headers.push((name.into(), "1".into()));
            }
        }
        headers
    }

    /// The request a letter's headers and body describe.
    fn from_letter(data: &[u8], body: &str) -> Option<Self> {
        let kind = PumpActionKind::parse(&header_value(data, PUMP_ACTION_HEADER)?)?;
        let flag = |name: &str| header_value(data, name).is_some_and(|value| value == "1");
        Some(Self {
            kind,
            unit: header_value(data, UNIT_HEADER).filter(|unit| !unit.is_empty()),
            attempts: header_value(data, ATTEMPTS_HEADER).and_then(parse_attempts),
            fresh: flag(FRESH_HEADER),
            refund: flag(REFUND_HEADER),
            force: flag(FORCE_HEADER),
            target: header_value(data, TARGET_HEADER).filter(|target| !target.is_empty()),
            step: header_value(data, STEP_HEADER).filter(|step| !step.is_empty()),
            word: header_value(data, WORD_HEADER).filter(|word| !word.is_empty()),
            reason: body.trim().to_string(),
        })
    }
}

/// The attempt count a header carries, `None` when it is not a number.
fn parse_attempts(value: String) -> Option<u32> {
    match value.trim().parse() {
        Ok(attempts) => Some(attempts),
        Err(_) => None,
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
        let mut extras = request.headers();
        extras.push((PUMP_RECORD_HEADER.into(), letter.record.to_string()));
        extras.push((
            super::pump_record::PUMP_NEED_HEADER.into(),
            letter.need.to_string(),
        ));
        let body = if request.reason.is_empty() {
            format!("{} {}", request.kind.as_str(), letter.need)
        } else {
            request.reason.clone()
        };
        let result = self.send_with_extras(
            SendRequest {
                from: letter.from.to_string(),
                to: vec![letter.to.to_string()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: format!("pump {}: {} action", letter.need, request.kind.as_str()),
                body,
                role: "inbox".into(),
                kind: Some("mail".into()),
                reply_to: Some(letter.record.to_string()),
                depends_on: Vec::new(),
            },
            &extras,
        )?;
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
    /// finished dispatch has nothing left to act on. A posted letter that no
    /// longer parses as an action is skipped.
    ///
    /// # Errors
    /// Returns an error if the event log or a letter cannot be read.
    pub fn pump_pending_actions(
        &self,
        applied: &BTreeSet<String>,
    ) -> Result<Vec<PumpAction>, VivariumError> {
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
        let mut pending = Vec::new();
        for event in &events {
            if applied.contains(&event.content_id) {
                continue;
            }
            if let Some(action) = self.action_of(&storage, event, &open)? {
                pending.push(action);
            }
        }
        Ok(pending)
    }

    /// One posted action, when its record is open and its letter parses.
    fn action_of(
        &self,
        storage: &Storage,
        event: &MailspaceEvent,
        open: &BTreeSet<String>,
    ) -> Result<Option<PumpAction>, VivariumError> {
        let Some(record) = event
            .note
            .as_deref()
            .filter(|record| open.contains(*record))
        else {
            return Ok(None);
        };
        let data = storage.read_message(&event.message_id)?;
        let body = crate::extract::extract_text(&data)?;
        let Some(request) = PumpActionRequest::from_letter(&data, &body) else {
            return Ok(None);
        };
        Ok(Some(PumpAction {
            id: event.content_id.clone(),
            record: record.to_string(),
            need: header_value(&data, super::pump_record::PUMP_NEED_HEADER).unwrap_or_default(),
            date: event.occurred_at.clone(),
            request,
        }))
    }
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
