//! Read synced report inputs from an existing mailspace without opening a writer.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::VivariumError;
use crate::pump::attempt_read::{AttemptFields, AttemptStatusEvent};
use crate::storage::{MailspaceEvent, Storage};

use super::model::{AttemptRecord, NeedWindow, ReportData};

pub fn read(project: &Path) -> Result<ReportData, VivariumError> {
    let dir = project.join(".vivi");
    let storage = Storage::open_mailspace_read_only(&dir)?;
    let messages = storage.list_messages()?;
    let mut attempts: BTreeMap<String, AttemptFields> = BTreeMap::new();
    let mut status_events: Vec<AttemptStatusEvent> = Vec::new();
    for message in messages {
        let bytes = storage.read_message(&message.message_id)?;
        let body = text_body(&bytes);
        if let Some(attempt) = crate::pump::attempt_read::parse_record(&body) {
            keep_newest(&mut attempts, attempt);
        }
        if let Some(status) = crate::pump::attempt_read::parse_status_event(&body) {
            status_events.push(status);
        }
    }
    apply_status_events(&mut attempts, &status_events);
    let attempts: Vec<AttemptFields> = attempts.into_values().collect();
    let needs = need_windows(&storage, &attempts)?;
    Ok(ReportData {
        attempts: attempts.iter().map(AttemptRecord::from).collect(),
        needs,
    })
}

fn text_body(bytes: &[u8]) -> String {
    mail_parser::MessageParser::default()
        .parse(bytes)
        .and_then(|message| message.body_text(0).map(|body| body.into_owned()))
        .unwrap_or_default()
}

fn keep_newest(attempts: &mut BTreeMap<String, AttemptFields>, next: AttemptFields) {
    let replace = attempts
        .get(&next.attempt)
        .is_none_or(|current| next.end.unwrap_or_default() > current.end.unwrap_or_default());
    if replace {
        attempts.insert(next.attempt.clone(), next);
    }
}

fn apply_status_events(
    attempts: &mut BTreeMap<String, AttemptFields>,
    events: &[AttemptStatusEvent],
) {
    for event in events {
        if let Some(attempt) = attempts.get_mut(&event.attempt) {
            attempt.apply_status_event(event);
        }
    }
}

fn need_windows(
    storage: &Storage,
    attempts: &[AttemptFields],
) -> Result<Vec<NeedWindow>, VivariumError> {
    let mut needs: std::collections::BTreeSet<String> = attempts
        .iter()
        .filter_map(|attempt| attempt.need.clone())
        .collect();
    needs.extend(
        storage
            .list_messages_by_role("needs")?
            .into_iter()
            .map(|message| message.handle),
    );
    needs
        .iter()
        .map(|need| need_window(storage, need))
        .collect()
}

fn need_window(storage: &Storage, need: &str) -> Result<NeedWindow, VivariumError> {
    let id = storage.resolve_message_token(need)?;
    let view = storage.message_by_id(&id)?;
    let opened_at = view
        .as_ref()
        .and_then(|message| crate::pump::attempt_read::opened_epoch(&message.date));
    let done_at = storage
        .list_mailspace_events(&id)?
        .iter()
        .filter(|event: &&MailspaceEvent| event.event_type == "done")
        .filter_map(|event| crate::pump::attempt_read::opened_epoch(&event.occurred_at))
        .max();
    let done = done_at.is_some()
        || view
            .as_ref()
            .is_some_and(|message| message.local_role == "done");
    Ok(NeedWindow {
        need: need.to_owned(),
        opened_at,
        done_at,
        done,
    })
}

impl From<&AttemptFields> for AttemptRecord {
    fn from(fields: &AttemptFields) -> Self {
        Self {
            need: fields.need.clone().unwrap_or_default(),
            task: fields.task.clone(),
            attempt: fields.attempt.clone(),
            actor: fields.actor.clone(),
            role: fields.role.clone(),
            pool: fields.pool.clone(),
            slot: fields.slot.clone(),
            model: fields.model.clone(),
            kind: fields.kind.clone(),
            start: fields.start,
            end: fields.end,
            agent_seconds: fields.agent_seconds,
            outcome: fields.outcome.clone(),
            ready_at: fields.ready_at,
            delivered_at: fields.delivered_at,
            merged_at: fields.merged_at,
        }
    }
}

#[cfg(test)]
#[path = "data_test.rs"]
mod tests;
