//! Rows for dispatches whose lifecycle is visible in mailspace records.

use std::collections::BTreeMap;
use std::path::Path;

use crate::pool::config::PoolConfig;

use super::events::{FinishedDispatch, ProjectEvents};
use super::model::{MonitorRow, RowKind, RowState};
use super::slots::slot_tail;

pub fn rows(
    project: &str,
    project_root: &Path,
    pools: &BTreeMap<String, PoolConfig>,
    events: &ProjectEvents,
    now: u64,
) -> Vec<MonitorRow> {
    let mut rows = events
        .open
        .iter()
        .filter_map(|open| open_row(project, open, pools, events, now))
        .collect::<Vec<_>>();
    rows.extend(
        events
            .finished
            .iter()
            .filter_map(|done| finished_row(project, project_root, done, pools)),
    );
    rows
}

fn open_row(
    project: &str,
    open: &crate::pool::record::OpenDispatch,
    pools: &BTreeMap<String, PoolConfig>,
    events: &ProjectEvents,
    now: u64,
) -> Option<MonitorRow> {
    let config = pools.get(&open.record.pool);
    if config.is_some_and(|config| config.provider == "local") {
        return None;
    }
    let info = events.task_info.get(&open.task);
    Some(MonitorRow {
        project: project.to_owned(),
        kind: RowKind::Slot,
        pool: open.record.pool.clone(),
        slot: open.record.slot.clone(),
        provider: config.map_or_else(|| "gone".into(), |item| item.provider.clone()),
        role: info.map_or_else(String::new, |item| item.role.clone()),
        task: Some(open.task.clone()),
        subject: info.map(|item| item.subject.clone()),
        model: open.record.model.clone(),
        state: RowState::Running,
        started_at: Some(open.record.at),
        running_secs: Some(now.saturating_sub(open.record.at)),
        budget_secs: Some(open.record.budget),
        read_only: Some(open.record.read_only),
        remote: true,
        tail: None,
    })
}

fn finished_row(
    project: &str,
    root: &Path,
    done: &FinishedDispatch,
    pools: &BTreeMap<String, PoolConfig>,
) -> Option<MonitorRow> {
    let config = pools.get(&done.pool)?;
    let local = config.provider == "local";
    Some(MonitorRow {
        project: project.to_owned(),
        kind: RowKind::Slot,
        pool: done.pool.clone(),
        slot: done.slot.clone(),
        provider: config.provider.clone(),
        role: done.role.clone(),
        task: Some(done.task.clone()),
        subject: Some(done.subject.clone()),
        model: done.model.clone(),
        state: RowState::Finished(done.outcome.clone()),
        started_at: Some(done.started_at),
        running_secs: Some(
            crate::pump::attempt_read::opened_epoch(&done.ended_at)
                .unwrap_or(done.started_at)
                .saturating_sub(done.started_at),
        ),
        budget_secs: Some(done.budget_secs),
        read_only: Some(done.read_only),
        remote: !local,
        tail: if local {
            slot_tail(&local_root(root, config).join(&done.machine))
        } else {
            None
        },
    })
}

fn local_root(project: &Path, config: &PoolConfig) -> std::path::PathBuf {
    let root = crate::pool::local_slots::expand_root(config.local.root_or_default());
    if root.is_absolute() {
        root
    } else {
        project.join(root)
    }
}

#[cfg(test)]
#[path = "lanes_test.rs"]
mod tests;
