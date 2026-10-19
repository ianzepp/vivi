//! Read-only pump service headers.

use std::fs::File;
use std::io::Read;
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
        read_only: None,
        remote: false,
        tail: existing(home.join("log.txt")),
    }
}

pub fn row(project: &str, home: &Path, _now: u64) -> Result<MonitorRow, crate::VivariumError> {
    refuse_missing_wal_index(home)?;
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
        read_only: None,
        remote: false,
        tail: existing(home.join("log.txt")),
    })
}

fn refuse_missing_wal_index(home: &Path) -> Result<(), crate::VivariumError> {
    let db = home.join(crate::pump::store::DB_FILENAME);
    let shm = home.join(format!("{}-shm", crate::pump::store::DB_FILENAME));
    if !db.is_file() || shm.exists() {
        return Ok(());
    }
    let Ok(mut file) = File::open(&db) else {
        return Ok(());
    };
    let mut header = [0_u8; 20];
    if file.read_exact(&mut header).is_ok() && header[18] == 2 && header[19] == 2 {
        return Err(crate::VivariumError::Other(format!(
            "pump monitor inspection skipped: {}-shm is missing; refusing to create it",
            db.display()
        )));
    }
    Ok(())
}

fn existing(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

#[cfg(test)]
#[path = "pump_test.rs"]
mod tests;
