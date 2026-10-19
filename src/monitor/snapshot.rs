//! Machine-wide snapshots from registered project files and read-only stores.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{events, lanes, model, pump, slots};

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub rows: Vec<model::MonitorRow>,
    pub warnings: Vec<String>,
}

pub fn merge_roots(registered: BTreeSet<PathBuf>, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    registered
        .into_iter()
        .chain(extra.iter().cloned())
        .filter_map(|path| {
            let canonical = path.canonicalize().unwrap_or(path);
            seen.insert(canonical.clone()).then_some(canonical)
        })
        .collect()
}

pub fn snapshot(roots: &[PathBuf], now: u64) -> Snapshot {
    let mut out = Snapshot {
        rows: Vec::new(),
        warnings: Vec::new(),
    };
    for root in roots {
        match project_rows(root, now) {
            Ok((mut rows, mut warnings)) => {
                out.rows.append(&mut rows);
                out.warnings.append(&mut warnings);
            }
            Err(error) => out.warnings.push(format!("{}: {error}", root.display())),
        }
    }
    out.rows.sort_by(row_order);
    out
}

fn project_rows(root: &Path, now: u64) -> Result<(Vec<model::MonitorRow>, Vec<String>), String> {
    let dir = root.join(".vivi");
    let text = fs::read_to_string(dir.join("mailspace.toml")).map_err(|e| e.to_string())?;
    let config: crate::mailspace::MailspaceConfig =
        toml::from_str(&text).map_err(|e| e.to_string())?;
    let id = fs::read_to_string(dir.join("mailspace.id"))
        .ok()
        .or(config.id.clone())
        .unwrap_or_default();
    let (events, event_warnings) = match events::read(&dir.join("mail.sqlite"), 20) {
        Ok(events) => (events, Vec::new()),
        Err(error) => (
            events::ProjectEvents::default(),
            vec![format!("{} events: {error}", root.display())],
        ),
    };
    let (rows, mut warnings) = collect_rows(root, &config, id.trim(), &events, now)?;
    warnings.extend(event_warnings);
    Ok((rows, warnings))
}

fn collect_rows(
    root: &Path,
    config: &crate::mailspace::MailspaceConfig,
    id: &str,
    events: &events::ProjectEvents,
    now: u64,
) -> Result<(Vec<model::MonitorRow>, Vec<String>), String> {
    let mut rows = Vec::new();
    let (mut local, mut warnings) = slots::rows(&config.name, root, id, &config.pools, events, now);
    rows.append(&mut local);
    rows.extend(lanes::rows(
        &config.name,
        root,
        id,
        &config.pools,
        events,
        now,
    ));
    if let Some(pump_config) = &config.pump {
        let (mut pump_rows, mut pump_warnings) =
            pump::rows(&config.name, &pump_config.home_dir(root), now);
        rows.append(&mut pump_rows);
        warnings.append(&mut pump_warnings);
    }
    Ok((rows, warnings))
}

fn row_order(a: &model::MonitorRow, b: &model::MonitorRow) -> std::cmp::Ordering {
    match (model::is_finished(a), model::is_finished(b)) {
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        _ => b.started_at.cmp(&a.started_at),
    }
}

#[cfg(test)]
#[path = "snapshot_test.rs"]
mod tests;
