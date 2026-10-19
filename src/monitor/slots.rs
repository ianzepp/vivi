//! Local slot rows, read from the provider's slot directories and mailspace events.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::pool::config::PoolConfig;
use crate::pool::slot::{self, PoolId};

use super::events::ProjectEvents;
use super::model::{MonitorRow, RowKind, RowState};

pub fn rows(
    project: &str,
    project_root: &Path,
    mailspace_id: &str,
    pools: &BTreeMap<String, PoolConfig>,
    events: &ProjectEvents,
    now: u64,
) -> (Vec<MonitorRow>, Vec<String>) {
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    for (name, config) in pools.iter().filter(|(_, c)| c.provider == "local") {
        match pool_rows(
            project,
            project_root,
            mailspace_id,
            name,
            config,
            events,
            now,
        ) {
            Ok(mut found) => rows.append(&mut found),
            Err(error) => warnings.push(format!("{project} ({name}): {error}")),
        }
    }
    (rows, warnings)
}

fn pool_rows(
    project: &str,
    project_root: &Path,
    mailspace_id: &str,
    name: &str,
    config: &PoolConfig,
    events: &ProjectEvents,
    now: u64,
) -> Result<Vec<MonitorRow>, crate::VivariumError> {
    let mailspace = slot::space_key(project, mailspace_id);
    let site = crate::pool::local_site::resolve(
        config.local.root_or_default(),
        project_root,
        &mailspace,
        name,
    )?;
    let machines = crate::pool::local_slots::list(&site)?;
    let id = PoolId::keyed(&mailspace, name);
    let mut slots = slot::pool_slots(&id, &machines);
    crate::pool::run::annotate_running(&mut slots, &events.open, now);
    Ok(slots
        .iter()
        .filter(|item| item.state == "busy" || item.state == "finished-uncollected")
        .map(|item| row(project, &site.pool_dir, item, events, now))
        .collect())
}

fn row(
    project: &str,
    root: &Path,
    slot: &slot::SlotStatus,
    events: &ProjectEvents,
    now: u64,
) -> MonitorRow {
    let dispatch = events
        .open
        .iter()
        .find(|open| open.record.machine == slot.id);
    let task = dispatch
        .map(|open| open.task.clone())
        .or_else(|| slot.task.clone());
    let info = task
        .as_ref()
        .and_then(|handle| events.task_info.get(handle));
    let need = dispatch
        .and_then(|open| open.record.need.clone())
        .or_else(|| info.and_then(|item| item.need.clone()));
    let work_kind = dispatch
        .and_then(|open| open.record.kind.clone())
        .or_else(|| info.and_then(|item| item.kind.clone()));
    let started_at = dispatch.map(|open| open.record.at).or(slot.claimed_at);
    MonitorRow {
        project: project.to_owned(),
        kind: RowKind::Slot,
        pool: slot.pool.clone(),
        slot: slot.name.clone(),
        provider: "local".into(),
        role: info.map_or_else(String::new, |item| item.role.clone()),
        task,
        need,
        work_kind,
        subject: info.map(|item| item.subject.clone()),
        model: dispatch.map_or_else(String::new, |open| open.record.model.clone()),
        state: if slot.state == "busy" {
            RowState::Running
        } else {
            RowState::Finished("uncollected".into())
        },
        started_at,
        running_secs: slot
            .running_secs
            .or_else(|| started_at.map(|at| now.saturating_sub(at))),
        budget_secs: slot
            .budget_secs
            .or_else(|| dispatch.map(|open| open.record.budget)),
        read_only: slot
            .read_only
            .or_else(|| dispatch.map(|open| open.record.read_only)),
        remote: false,
        tail: Path::new(&slot.id)
            .file_name()
            .and_then(|name| slot_tail(&root.join(name))),
    }
}

/// Resolve the slot directory named by a dispatch receipt under its configured pool.
/// The receipt's machine id must match the current local-site layout exactly.
pub fn local_slot_dir(
    project: &str,
    project_root: &Path,
    mailspace_id: &str,
    pool: &str,
    config: &PoolConfig,
    machine: &str,
) -> Option<PathBuf> {
    let mailspace = slot::space_key(project, mailspace_id);
    let site = crate::pool::local_site::resolve(
        config.local.root_or_default(),
        project_root,
        &mailspace,
        pool,
    )
    .ok()?;
    if !crate::pool::local_slots::slot_id_ok(machine) {
        return None;
    }
    let slot_name = Path::new(machine).file_name()?.to_str()?;
    let expected = if site.relative {
        format!("{pool}/{slot_name}")
    } else {
        format!("{mailspace}/{pool}/{slot_name}")
    };
    (machine == expected).then(|| site.pool_dir.join(slot_name))
}

pub fn slot_tail(slot_dir: &Path) -> Option<PathBuf> {
    ["job/agent.out", "job/run.log", "boot.out", "boot.log"]
        .into_iter()
        .map(|file| slot_dir.join(file))
        .find(|path| path.is_file())
}

#[cfg(test)]
#[path = "slots_test.rs"]
mod tests;
