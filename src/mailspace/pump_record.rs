//! Pump records: the `pump` item kind, a sibling of task, need and want.
//!
//! One dispatch is one pump record. A record names exactly one need (the
//! `need` of its [`RecordFront`] front matter), is addressed to the pump's own identity (`[pump]
//! reply_from`, the identity the pump reads needs as and replies as), and
//! covers that dispatch end to end. An open record alone admits a need: the
//! need's own addressee plays no part. Redispatching the same need is a new
//! record, so a need's pump history is its list of records. At most one
//! record per need is open.
//!
//! A record lives in the `pumps` folder while open and moves to `done` when
//! closed. It is the pump's own record, not backlog work: it mints no node in
//! the `backlog` graph, so `vivi step` and the Mind never see it, and it is
//! local to this mailspace (sync skips it). Closing carries an optional
//! outcome note (`landed`, `gave-up`, `withdrawn`, `superseded`, or any text)
//! stored on the `pump done` event.

use serde::Serialize;

use super::backlog::canonical_content_handle;
use super::event_log::log_move_event;
use super::thread::MailspaceThreadMessage;
use super::{Mailspace, SendRequest};
use crate::error::VivariumError;
use crate::letter::records::{RecordFront, record_letter};
use crate::storage::{MailspaceEvent, Storage, StoredMessageView};

/// Where a pump record stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PumpState {
    Open,
    Done,
}

impl PumpState {
    /// The word the CLI prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
        }
    }
}

/// One pump record as the CLI lists it.
#[derive(Debug, Clone, Serialize)]
pub struct PumpRecord {
    pub handle: String,
    pub kind: String,
    /// The need this dispatch covers.
    pub need: String,
    pub status: PumpState,
    pub from: String,
    pub to: String,
    pub date: String,
    pub subject: String,
    /// The close note of a done record; reopening clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Why the stored letter did not parse as a record (it names the handle);
    /// `need` is empty then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub letter_error: Option<String>,
}

impl Mailspace {
    /// The sender a pump record defaults to (`[pump] from`) and the pump's own
    /// identity it is addressed to (`[pump] reply_from`).
    pub(super) fn pump_names(&self) -> (String, String) {
        match &self.config.pump {
            Some(pump) => (pump.from.clone(), pump.reply_from.clone()),
            None => {
                let defaults = crate::pump::config::PumpConfig::default();
                (defaults.from, defaults.reply_from)
            }
        }
    }

    /// File a new open pump record for `need`, addressed to the pump.
    ///
    /// The one-open-record check and the send run in one store write
    /// transaction (`BEGIN IMMEDIATE`), so two concurrent sends, from this
    /// process or another, cannot both pass the check: the second waits for
    /// the first to commit, then finds its record open and is refused.
    ///
    /// # Errors
    /// Returns an error if the need is unknown or not a need, if the need
    /// already has an open pump record (the error names it), if an identity
    /// does not resolve, or if the send fails. A failed send leaves no record.
    pub fn pump_send(
        &self,
        from: Option<&str>,
        need: &str,
        body: Option<&str>,
    ) -> Result<PumpRecord, VivariumError> {
        let storage = self.storage()?;
        in_write_batch(&storage, || self.send_checked(&storage, from, need, body))
    }

    /// The body of [`Mailspace::pump_send`], run inside the write batch.
    fn send_checked(
        &self,
        storage: &Storage,
        from: Option<&str>,
        need: &str,
        body: Option<&str>,
    ) -> Result<PumpRecord, VivariumError> {
        let (default_from, to) = self.pump_names();
        let (need_handle, need_subject) = self.resolve_need(storage, need)?;
        let need_id = storage.resolve_message_token(&need_handle)?;
        let need_body = crate::extract::extract_text(&storage.read_message(&need_id)?)?;
        crate::letter::read::need_of(&need_handle, &need_body)
            .map_err(|error| VivariumError::Message(format!("need {need_handle}: {error}")))?;
        if let Some(open) = self.open_record_of(storage, &need_handle)? {
            return Err(VivariumError::Message(format!(
                "need {need_handle} already has an open pump record {}; close it with `vivi pump done {}` first",
                open.handle, open.handle
            )));
        }
        let body = body
            .map(str::trim)
            .filter(|body| !body.is_empty())
            .map_or_else(
                || format!("Dispatch of need {need_handle}."),
                str::to_string,
            );
        let (mut front, prose) = record_letter(&need_handle, &body)
            .map_err(|error| VivariumError::Message(error.to_string()))?;
        if front.need != need_handle {
            let (named, _) = self.resolve_need(storage, &front.need)?;
            if named != need_handle {
                return Err(VivariumError::Message(format!(
                    "pump record front matter names need {}, not {need_handle}",
                    front.need
                )));
            }
        }
        front.need.clone_from(&need_handle);
        let letter = crate::letter::render(&front, &prose)
            .map_err(|error| VivariumError::Message(error.to_string()))?;
        self.check_actionable(storage, &front, &letter)?;
        let result = self.send(SendRequest {
            from: from.map_or(default_from, str::to_string),
            to: vec![to],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("pump {need_handle}: {need_subject}"),
            body: letter,
            role: "pumps".into(),
            kind: Some("pump".into()),
            reply_to: None,
        })?;
        let delivered = result
            .delivered
            .first()
            .ok_or_else(|| VivariumError::Message("the pump record was not delivered".into()))?;
        let row = self.record_row(storage, &delivered.handle)?;
        self.record_of(storage, &row)
    }

    /// Refuse a record the flow could not act on: a closed need, or a repair
    /// record without a known target.
    fn check_actionable(
        &self,
        storage: &Storage,
        front: &RecordFront,
        rendered: &str,
    ) -> Result<(), VivariumError> {
        let need = &front.need;
        let (done, marks) = need_facts(storage, need)?;
        if done {
            return Err(VivariumError::Message(format!(
                "need {need} is done: a pump record needs an open need"
            )));
        }
        let kind = crate::pump::flow::kind::of(Some(rendered), &marks);
        if kind != crate::pump::flow::def::FlowKind::Repair {
            return Ok(());
        }
        let target = front
            .target
            .as_deref()
            .map(str::trim)
            .filter(|word| !word.is_empty());
        let known: Option<Vec<String>> = self
            .config
            .pump
            .as_ref()
            .map(|pump| pump.target_configs().into_keys().collect());
        let listed = known
            .as_ref()
            .map_or_else(|| "a configured target".to_string(), |ids| ids.join(", "));
        let accepted = target.is_some_and(|word| {
            known
                .as_ref()
                .is_none_or(|ids| ids.iter().any(|id| id == word))
        });
        if accepted {
            return Ok(());
        }
        Err(VivariumError::Message(format!(
            "repair record for need {need}: expected `target` in the record front matter naming one of {listed}; actual: {}",
            target.unwrap_or("none")
        )))
    }

    /// The open pump record of `need`, when it has one.
    pub(super) fn open_record_of(
        &self,
        storage: &Storage,
        need: &str,
    ) -> Result<Option<PumpRecord>, VivariumError> {
        Ok(self
            .pump_records(storage, Some(PumpState::Open))?
            .into_iter()
            .find(|record| record.need == need))
    }

    /// Every pump record, oldest first, optionally for one need and one state.
    ///
    /// # Errors
    /// Returns an error if `need` is unknown or not a need, or storage fails.
    pub fn pump_list(
        &self,
        need: Option<&str>,
        state: Option<PumpState>,
    ) -> Result<Vec<PumpRecord>, VivariumError> {
        let storage = self.storage()?;
        let need = need
            .map(|token| self.resolve_need(&storage, token).map(|(handle, _)| handle))
            .transpose()?;
        Ok(self
            .pump_records(&storage, state)?
            .into_iter()
            .filter(|record| need.as_ref().is_none_or(|need| &record.need == need))
            .collect())
    }

    /// The open pump record of `need`, if it has one.
    ///
    /// # Errors
    /// Same as [`Mailspace::pump_list`].
    pub fn pump_open_record(&self, need: &str) -> Result<Option<PumpRecord>, VivariumError> {
        Ok(self
            .pump_list(Some(need), Some(PumpState::Open))?
            .into_iter()
            .next())
    }

    /// A need's pump history: all its records, oldest first, with state.
    ///
    /// # Errors
    /// Same as [`Mailspace::pump_list`].
    pub fn pump_history(&self, need: &str) -> Result<Vec<PumpRecord>, VivariumError> {
        self.pump_list(Some(need), None)
    }

    /// The messages of every thread rooted at one of `need`'s pump records,
    /// oldest record first: the records and the replies the pump posted on
    /// them (plan, attempt and merge notices).
    ///
    /// # Errors
    /// Same as [`Mailspace::pump_list`], or a thread cannot be read.
    pub fn pump_threads(&self, need: &str) -> Result<Vec<MailspaceThreadMessage>, VivariumError> {
        let mut messages = Vec::new();
        for record in self.pump_history(need)? {
            messages.extend(self.thread(&record.handle, false, usize::MAX, usize::MAX)?);
        }
        Ok(messages)
    }

    /// One pump record and its body.
    ///
    /// # Errors
    /// Returns an error if `handle` is not a pump record or storage fails.
    pub fn pump_show(&self, handle: &str) -> Result<(PumpRecord, String), VivariumError> {
        let storage = self.storage()?;
        let row = self.record_row(&storage, handle)?;
        let body = crate::extract::extract_text(&storage.read_message(&row.message_id)?)?;
        Ok((self.record_of(&storage, &row)?, body))
    }

    /// Close an open pump record, with an optional outcome note.
    ///
    /// # Errors
    /// Returns an error if `handle` is not a pump record, if it is already
    /// done, or if storage fails.
    pub fn pump_done(&self, handle: &str, note: Option<&str>) -> Result<PumpRecord, VivariumError> {
        let storage = self.storage()?;
        let row = self.record_row(&storage, handle)?;
        if row.local_role != "pumps" {
            return Err(VivariumError::Message(format!(
                "pump record {handle} is already done"
            )));
        }
        let note = note.map(str::trim).filter(|note| !note.is_empty());
        let actor = self.row_holder(&row);
        storage.move_message_to_role(&row.account, &row.message_id, "done")?;
        log_move_event(&storage, &actor, "done", &row, "pump done", note)?;
        let moved = self.record_row(&storage, handle)?;
        self.record_of(&storage, &moved)
    }

    /// Reopen a done pump record, unless another record of its need is open.
    ///
    /// # Errors
    /// Returns an error if `handle` is not a pump record, if it is not done,
    /// if another record for the same need is open (the error names it), or
    /// if storage fails.
    pub fn pump_reopen(&self, handle: &str) -> Result<PumpRecord, VivariumError> {
        let storage = self.storage()?;
        in_write_batch(&storage, || {
            let row = self.record_row(&storage, handle)?;
            if row.local_role != "done" {
                return Err(VivariumError::Message(format!(
                    "pump record {handle} is not done"
                )));
            }
            let record = self.record_of(&storage, &row)?;
            if let Some(open) = self.open_record_of(&storage, &record.need)? {
                return Err(VivariumError::Message(format!(
                    "need {} already has an open pump record {}; close it first",
                    record.need, open.handle
                )));
            }
            let actor = self.row_holder(&row);
            storage.move_message_to_role(&row.account, &row.message_id, "pumps")?;
            log_move_event(&storage, &actor, "pumps", &row, "pump reopen", None)?;
            let moved = self.record_row(&storage, handle)?;
            self.record_of(&storage, &moved)
        })
    }

    /// The identity that holds a row, else its raw account name.
    fn row_holder(&self, row: &StoredMessageView) -> String {
        self.identity_owning_account(&row.account)
            .unwrap_or_else(|| row.account.clone())
    }

    /// A need's canonical handle and subject. Refuses an unknown token and
    /// anything that is not a need.
    pub(super) fn resolve_need(
        &self,
        storage: &Storage,
        token: &str,
    ) -> Result<(String, String), VivariumError> {
        let message_id = storage
            .resolve_message_token(token)
            .map_err(|error| VivariumError::Message(format!("unknown need '{token}': {error}")))?;
        let view = storage
            .message_by_id(&message_id)?
            .ok_or_else(|| VivariumError::Message(format!("unknown need '{token}'")))?;
        let kind = self.source_kind(storage, &view)?;
        if kind != "need" {
            return Err(VivariumError::Message(format!(
                "{token} is {kind}, not need; a pump record covers one need"
            )));
        }
        Ok((
            canonical_content_handle(storage, &message_id)?,
            view.subject,
        ))
    }

    /// The holder row of the pump record `token` names. A sender's `sent` copy
    /// resolves to the record's holder row.
    fn record_row(
        &self,
        storage: &Storage,
        token: &str,
    ) -> Result<StoredMessageView, VivariumError> {
        let not_record =
            |kind: &str| VivariumError::Message(format!("{token} is {kind}, not a pump record"));
        let message_id = storage.resolve_message_token(token)?;
        let view = storage
            .message_by_id(&message_id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {token}")))?;
        let kind = self.source_kind(storage, &view)?;
        if kind != "pump" {
            return Err(not_record(&kind));
        }
        if matches!(view.local_role.as_str(), "pumps" | "done") {
            return Ok(view);
        }
        storage
            .live_messages_for_content(&view.content_id)?
            .into_iter()
            .find(|row| matches!(row.local_role.as_str(), "pumps" | "done"))
            .ok_or_else(|| not_record("a copy that is not held as a record"))
    }

    /// Every pump record in `state` (all when `None`), oldest first: the
    /// holder rows of every `pump send` delivery, in `pumps` (open) or `done`.
    /// Rows outside `state` are skipped before their letters are read, so
    /// listing the open records stays cheap as closed history grows.
    pub(super) fn pump_records(
        &self,
        storage: &Storage,
        state: Option<PumpState>,
    ) -> Result<Vec<PumpRecord>, VivariumError> {
        let mut records = Vec::new();
        for event in storage.list_mailspace_events_for_command("pump send", "delivered")? {
            let Some(row) = storage.message_by_id(&event.message_id)? else {
                continue;
            };
            let held = match row.local_role.as_str() {
                "pumps" => PumpState::Open,
                "done" => PumpState::Done,
                _ => continue,
            };
            if state.is_none_or(|state| state == held) {
                records.push(self.record_of(storage, &row)?);
            }
        }
        Ok(records)
    }

    fn record_of(
        &self,
        storage: &Storage,
        row: &StoredMessageView,
    ) -> Result<PumpRecord, VivariumError> {
        let data = storage.read_message(&row.message_id)?;
        let status = if row.local_role == "done" {
            PumpState::Done
        } else {
            PumpState::Open
        };
        let events = storage.list_mailspace_events(&row.message_id)?;
        let handle = storage.display_handle(&row.message_id)?;
        let body = crate::extract::extract_text(&data)?;
        let (need, letter_error) = match crate::letter::read::record_of(&handle, &body) {
            Ok(envelope) => (envelope.front.need, None),
            Err(error) => (String::new(), Some(error.to_string())),
        };
        Ok(PumpRecord {
            handle,
            kind: "pump".into(),
            need,
            status,
            from: row.from_addr.clone(),
            to: row.to_addr.clone(),
            date: row.date.clone(),
            subject: row.subject.clone(),
            outcome: (status == PumpState::Done)
                .then(|| outcome_of(&events))
                .flatten(),
            letter_error,
        })
    }
}

/// Run `work` inside one store write transaction: committed when it
/// succeeds, rolled back when it fails, so a refused or failed step leaves
/// the mailspace as it was and concurrent writers are serialized.
pub(super) fn in_write_batch<T>(
    storage: &Storage,
    work: impl FnOnce() -> Result<T, VivariumError>,
) -> Result<T, VivariumError> {
    storage.begin_batch()?;
    match work() {
        Ok(value) => {
            storage.commit_batch()?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback) = storage.rollback_batch() {
                return Err(VivariumError::Message(format!(
                    "{error}; the rollback also failed: {rollback}"
                )));
            }
            Err(error)
        }
    }
}

/// Whether a need is closed, and its folded marks, read from the events of
/// every live row of the item (the newest folder move decides closed).
fn need_facts(storage: &Storage, need: &str) -> Result<(bool, Vec<String>), VivariumError> {
    let item = super::item_facts::held_item_if_present(storage, need)?
        .ok_or_else(|| VivariumError::Message(format!("unknown need '{need}'")))?;
    Ok((
        super::item_facts::is_done(&item),
        super::item_facts::sorted_marks(&item),
    ))
}

/// The close note: the newest `pump done` note, cleared by a later reopen.
fn outcome_of(events: &[MailspaceEvent]) -> Option<String> {
    let mut outcome = None;
    for event in events {
        match event.command.as_str() {
            "pump done" => outcome.clone_from(&event.note),
            "pump reopen" => outcome = None,
            _ => {}
        }
    }
    outcome
}

#[cfg(test)]
#[path = "pump_record_test.rs"]
mod tests;
