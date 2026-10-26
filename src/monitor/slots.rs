//! Local slot rows, read from the provider's slot directories and mailspace events.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::pool::collect::JobResult;
use crate::pool::config::PoolConfig;
use crate::pool::record::OpenDispatch;
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
    let (finished_at, running_secs) = finish_times(root, slot, dispatch, started_at, now);
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
        stale_age_secs: None,
        finished_at,
        running_secs,
        budget_secs: slot
            .budget_secs
            .or_else(|| dispatch.map(|open| open.record.budget)),
        read_only: slot
            .read_only
            .or_else(|| dispatch.map(|open| open.record.read_only)),
        remote: false,
        tail: slot_dir(root, slot).and_then(|dir| slot_tail(&dir)),
    }
}

/// Completion and duration for one row. A stopped slot with an open dispatch
/// may carry a validated current-attempt result artifact; when it does, its
/// completion epoch is authoritative and the duration stops there instead of
/// running to `now`. Running slots never read an artifact.
fn finish_times(
    root: &Path,
    slot: &slot::SlotStatus,
    dispatch: Option<&OpenDispatch>,
    started_at: Option<u64>,
    now: u64,
) -> (Option<u64>, Option<u64>) {
    let finished_at = if slot.state == "busy" {
        None
    } else {
        slot_dir(root, slot).and_then(|dir| {
            dispatch.and_then(|open| artifact_completion(&dir, open, started_at, now))
        })
    };
    let running_secs = finished_at
        .zip(started_at)
        .map(|(done, at)| done.saturating_sub(at))
        .or(slot.running_secs)
        .or_else(|| started_at.map(|at| now.saturating_sub(at)));
    (finished_at, running_secs)
}

/// The on-disk directory of a local slot: the pool dir joined to the slot's
/// directory name (the last component of its id), matching `slot_tail`.
fn slot_dir(root: &Path, slot: &slot::SlotStatus) -> Option<PathBuf> {
    Path::new(&slot.id).file_name().map(|name| root.join(name))
}

/// Validated completion epoch from a stopped slot's `job/result` artifact,
/// written by the runner as key=value lines with no absolute completion time.
/// The artifact counts only when it provably belongs to the current attempt:
/// `state=done`, a `job` identity matching the open dispatch, and any dispatch
/// target/repository echoed unchanged. The file's modification time then
/// supplies the completion epoch, which must sit inside the attempt window:
/// not before the attempt started, not a future completion relative to `now`.
/// A claim or start time is never used as a completion; an artifact that
/// cannot be validated leaves the finish unknown so session history can infer
/// a first-observed one.
fn artifact_completion(
    slot_dir: &Path,
    dispatch: &OpenDispatch,
    started_at: Option<u64>,
    now: u64,
) -> Option<u64> {
    let path = slot_dir.join("job/result");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return None,
    };
    let result = JobResult::parse(&String::from_utf8_lossy(&bytes));
    let echoes = |key: &str, want: Option<&String>| match want {
        Some(value) => result.get(key) == value.as_str(),
        None => true,
    };
    if !(result.get("state") == "done"
        && result.get("job") == dispatch.record.job
        && echoes("target", dispatch.record.target.as_ref())
        && echoes("repository", dispatch.record.repository.as_ref()))
    {
        return None;
    }
    match modification_epoch(&path) {
        Some(secs) => started_at
            .is_some_and(|at| secs >= at && secs <= now)
            .then_some(secs),
        None => None,
    }
}

/// When `path` was last modified, as epoch seconds; none when the metadata or
/// the timestamp cannot be read, or the time predates the epoch.
fn modification_epoch(path: &Path) -> Option<u64> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return None,
    };
    let modified = match metadata.modified() {
        Ok(time) => time,
        Err(_) => return None,
    };
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(age) => Some(age.as_secs()),
        Err(_) => None,
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
    let site = match crate::pool::local_site::resolve(
        config.local.root_or_default(),
        project_root,
        &mailspace,
        pool,
    ) {
        Ok(site) => site,
        Err(_) => return None,
    };
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
