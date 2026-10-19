//! Read-only pump service headers.

use std::path::{Path, PathBuf};

use super::model::{MonitorRow, RowKind, RowState};

pub fn row(project: &str, home: &Path, _now: u64) -> Result<MonitorRow, crate::VivariumError> {
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

fn existing(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

#[cfg(test)]
#[path = "pump_test.rs"]
mod tests;
