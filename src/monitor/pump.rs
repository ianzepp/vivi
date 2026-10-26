//! Read-only pump service headers.

use std::path::{Path, PathBuf};

use super::model::{MonitorRow, RowKind, RowState};

/// Service status plus recent actual tick rows from the bounded cycle log.
pub fn rows(project: &str, home: &Path, now: u64) -> (Vec<MonitorRow>, Vec<String>) {
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    match row(project, home, now) {
        Ok(row) => rows.push(row),
        Err(error) => warnings.push(format!("pump status: {error}")),
    }
    match crate::pump::cycle::read(home) {
        Ok(cycles) => rows.extend(
            cycles
                .into_iter()
                .map(|cycle| cycle_row(project, home, now, cycle)),
        ),
        Err(error) => warnings.push(format!("pump cycles: {error}")),
    }
    (rows, warnings)
}

fn cycle_row(
    project: &str,
    home: &Path,
    now: u64,
    cycle: crate::pump::cycle::TickRecord,
) -> MonitorRow {
    let state = if cycle.is_finished() {
        RowState::Finished(cycle.outcome.clone().unwrap_or_else(|| "finished".into()))
    } else {
        RowState::Running
    };
    MonitorRow {
        project: project.to_owned(),
        kind: RowKind::Pump,
        pool: "pump".into(),
        slot: "tick".into(),
        provider: "pump".into(),
        role: "cycle".into(),
        task: Some(cycle.id),
        need: None,
        work_kind: Some("tick".into()),
        subject: None,
        model: String::new(),
        state,
        started_at: Some(cycle.started_at),
        running_secs: Some(
            cycle
                .duration_secs
                .unwrap_or_else(|| now.saturating_sub(cycle.started_at)),
        ),
        budget_secs: None,
        finished_at: cycle.ended_at,
        stale_age_secs: None,
        read_only: None,
        remote: false,
        tail: existing(home.join("log.txt")),
    }
}

pub fn row(project: &str, home: &Path, _now: u64) -> Result<MonitorRow, crate::VivariumError> {
    let db = home.join(crate::pump::store::DB_FILENAME);
    crate::storage::read_only::inspect(
        &db,
        crate::storage::read_only::IndexPolicy::OwnerMayCreateIndex,
    )?;
    let control = crate::pump::control::read(home)?;
    let active = crate::pump::run::lock_is_held(home)?;
    let mode = control.mode.to_string();
    let state = if active {
        RowState::Running
    } else if mode == "running" {
        RowState::Finished("idle".into())
    } else {
        RowState::Finished(mode.clone())
    };
    Ok(MonitorRow {
        project: project.to_owned(),
        kind: RowKind::Pump,
        pool: "pump".into(),
        slot: String::new(),
        provider: "pump".into(),
        role: mode,
        task: None,
        need: None,
        work_kind: None,
        subject: None,
        model: String::new(),
        state,
        started_at: None,
        running_secs: None,
        budget_secs: None,
        finished_at: None,
        stale_age_secs: None,
        read_only: None,
        remote: false,
        tail: existing(home.join("log.txt")),
    })
}

fn existing(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

#[cfg(test)]
#[path = "pump_test.rs"]
mod tests;
