//! Reading half of the mailspace port: task, need and record state, marks,
//! bound units and the run count derived from bound sentinels.

use super::{MailReader, MailspacePort, NeedStatus, OpenRecords, TaskState, TaskStatus};
use crate::error::VivariumError;
use crate::mailspace::{Mailspace, PumpState, folded_marks};
use crate::pump::flow::def::SENTINEL_MARKER;
use crate::storage::{MailspaceEvent, Storage, StoredMessageView};

/// How many run sentinels `need` has: the bound tasks whose body starts with
/// the sentinel marker. This is the Q1 run count, derived from mail alone.
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
        if body.trim_start().starts_with(SENTINEL_MARKER) {
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
        let done = match moved {
            Some(event) => event.to_role.as_deref() == Some("done"),
            None => item.rows.iter().any(|row| row.local_role == "done"),
        };
        if !done {
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
        let done = match newest_move(&item.events) {
            Some(event) => event.to_role.as_deref() == Some("done"),
            None => item.rows.iter().any(|row| row.local_role == "done"),
        };
        Ok(if done {
            NeedStatus::Done
        } else {
            NeedStatus::Open
        })
    }

    fn need_marks(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        let storage = self.mailspace.storage()?;
        let mut events = held_item(&storage, need)?.events;
        events.sort_by(|a, b| (&a.occurred_at, a.event_id).cmp(&(&b.occurred_at, b.event_id)));
        Ok(folded_marks(&events))
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

/// Every live row of one item and every event logged against those rows.
struct HeldItem {
    rows: Vec<StoredMessageView>,
    events: Vec<MailspaceEvent>,
}

fn held_item(storage: &Storage, handle: &str) -> Result<HeldItem, VivariumError> {
    let message_id = storage.resolve_message_token(handle)?;
    let view = storage
        .message_by_id(&message_id)?
        .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))?;
    let rows = storage.live_messages_for_content(&view.content_id)?;
    let mut events = Vec::new();
    for row in &rows {
        events.extend(storage.list_mailspace_events(&row.message_id)?);
    }
    Ok(HeldItem { rows, events })
}

/// The newest folder move of the item, close or reopen alike.
fn newest_move(events: &[MailspaceEvent]) -> Option<&MailspaceEvent> {
    events
        .iter()
        .filter(|event| event.event_type == "moved" && event.to_role.is_some())
        .max_by(|a, b| (&a.occurred_at, a.event_id).cmp(&(&b.occurred_at, b.event_id)))
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
