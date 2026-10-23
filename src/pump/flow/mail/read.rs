//! Reading half of the mailspace port: task, need and record state, marks,
//! bound units and the run count derived from bound sentinels.

use super::{MailReader, MailspacePort, NeedStatus, OpenRecords, TaskState, TaskStatus};
use crate::error::VivariumError;
use crate::letter::read::step_of;
use crate::letter::work::StepFront;
use crate::mailspace::{Mailspace, PumpState, held_item, is_done, newest_move, sorted_marks};
use crate::storage::{Storage, StoredMessageView};

/// How many run sentinels `need` has: the bound tasks whose step table is a
/// sentinel. This is the Q1 run count, derived from mail alone.
///
/// # Errors
/// Returns an error when the bound units or their bodies cannot be read.
pub fn sentinel_count_of(mailspace: &Mailspace, need: &str) -> Result<usize, VivariumError> {
    let units = mailspace.backlog_bound_units(need)?;
    let storage = mailspace.storage()?;
    let mut count = 0;
    for unit in units {
        let message_id = storage.resolve_message_token(&unit.handle)?;
        let body = crate::extract::extract_text(&storage.read_message(&message_id)?)?;
        if step_of(&body).is_some_and(|step| step.sentinel) {
            count += 1;
        }
    }
    Ok(count)
}

/// The run count shown to the operator: how many sentinels `need` has bound.
/// A mailspace that cannot be read shows no runs.
#[must_use]
pub fn run_count(mailspace: &Mailspace, need: &str) -> usize {
    match sentinel_count_of(mailspace, need) {
        Ok(count) => count,
        Err(_) => 0,
    }
}

impl MailReader for MailspacePort {
    fn task_state(&self, handle: &str) -> Result<TaskState, VivariumError> {
        let storage = self.mailspace.storage()?;
        let item = held_item(&storage, handle)?;
        let moved = newest_move(&item.events);
        if !is_done(&item) {
            return Ok(TaskState {
                status: TaskStatus::Open,
                closer: None,
                verdict: None,
            });
        }
        Ok(TaskState {
            status: TaskStatus::Done,
            closer: moved.and_then(|event| event.actor_identity.clone()),
            verdict: verdict_of(&storage, &item.rows)?,
        })
    }

    fn need_state(&self, need: &str) -> Result<NeedStatus, VivariumError> {
        let storage = self.mailspace.storage()?;
        let item = held_item(&storage, need)?;
        Ok(if is_done(&item) {
            NeedStatus::Done
        } else {
            NeedStatus::Open
        })
    }

    fn need_marks(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        let storage = self.mailspace.storage()?;
        Ok(sorted_marks(&held_item(&storage, need)?))
    }

    fn open_record(&self, need: &str) -> Result<Option<(String, String)>, VivariumError> {
        let Some(record) = self.mailspace.pump_open_record(need)? else {
            return Ok(None);
        };
        let (record, body) = self.mailspace.pump_show(&record.handle)?;
        Ok(Some((record.handle, body)))
    }

    fn bound_units(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        Ok(self
            .mailspace
            .backlog_bound_units(need)?
            .into_iter()
            .map(|unit| unit.handle)
            .collect())
    }

    fn sentinel_count(&self, need: &str) -> Result<usize, VivariumError> {
        sentinel_count_of(&self.mailspace, need)
    }

    fn need_prerequisites(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        Ok(self
            .mailspace
            .backlog_dependencies(need)?
            .into_iter()
            .map(|dep| dep.handle)
            .collect())
    }

    fn last_closed_record(&self, need: &str) -> Result<Option<String>, VivariumError> {
        Ok(self
            .mailspace
            .pump_history(need)?
            .into_iter()
            .rev()
            .find(|record| record.status == PumpState::Done)
            .map(|record| record.outcome.unwrap_or_default()))
    }

    fn sent_step(&self, subject: &str, step: &StepFront) -> Result<Option<String>, VivariumError> {
        let sent = self
            .mailspace
            .list_kind(Some(&self.pump_identity), "sent", "task")?;
        let storage = self.mailspace.storage()?;
        let mut newest: Option<(String, String)> = None;
        for row in sent.iter().filter(|row| row.subject == subject) {
            let body = crate::extract::extract_text(&storage.read_message(&row.message_id)?)?;
            if step_of(&body).as_ref() != Some(step) {
                continue;
            }
            let handle = delivered_handle(&storage, row)?;
            if self.task_state(&handle)?.status != TaskStatus::Open {
                continue;
            }
            if newest.as_ref().is_none_or(|(date, _)| *date <= row.date) {
                newest = Some((row.date.clone(), handle));
            }
        }
        Ok(newest.map(|(_, handle)| handle))
    }
}

impl OpenRecords for MailspacePort {
    fn open_record_needs(&self) -> Result<Vec<(String, String)>, VivariumError> {
        let mut records = self.mailspace.pump_list(None, Some(PumpState::Open))?;
        records.sort_by(|a, b| a.date.cmp(&b.date));
        Ok(records
            .into_iter()
            .map(|record| (record.need, record.date))
            .collect())
    }
}

/// The handle `send_step` hands back for a sent copy: the recipient's copy
/// of the same content, or the sent copy itself when nobody local holds one.
fn delivered_handle(storage: &Storage, sent: &StoredMessageView) -> Result<String, VivariumError> {
    let rows = storage.live_messages_for_content(&sent.content_id)?;
    Ok(rows
        .into_iter()
        .find(|row| row.local_role != "sent")
        .map_or_else(|| sent.handle.clone(), |row| row.handle))
}

/// The verdict the close recorded, from whichever row carries it.
fn verdict_of(
    storage: &Storage,
    rows: &[StoredMessageView],
) -> Result<Option<String>, VivariumError> {
    for row in rows {
        let metadata = storage.item_metadata(&row.message_id)?;
        if let Some(verdict) = metadata
            .get("verdict")
            .filter(|verdict| !verdict.is_empty())
        {
            return Ok(Some(verdict.clone()));
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "read_test.rs"]
mod tests;
