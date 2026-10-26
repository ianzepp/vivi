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
    /// Projects whose observation failed wholesale, counted independently of
    /// warning-message delivery: every failed project adds one here even if
    /// its warning is later deduplicated or dropped.
    pub active_failures: usize,
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
        active_failures: 0,
    };
    for root in roots {
        match project_rows(root, now) {
            Ok(rows) => out.rows.extend(rows),
            Err(failure) => {
                out.active_failures += 1;
                out.warnings.push(failure.warning());
            }
        }
    }
    out.rows.sort_by(row_order);
    out
}

/// Observe one project atomically: all of its rows, or the failure naming
/// every source that could not be read. A failed source discards the
/// project's partial rows; other projects are observed regardless.
fn project_rows(root: &Path, now: u64) -> Result<Vec<model::MonitorRow>, ProjectFailure> {
    let dir = root.join(".vivi");
    let path = dir.join("mailspace.toml");
    let label = root.display().to_string();
    let text =
        fs::read_to_string(&path).map_err(|error| ProjectFailure::one(&label, "config", error))?;
    let config = crate::mailspace::inherit::load_config(&path, &text)
        .map_err(|error| ProjectFailure::one(&label, "config", error))?;
    let id = match fs::read_to_string(dir.join("mailspace.id")) {
        Ok(id) => id,
        Err(_) => config.id.clone().unwrap_or_default(),
    };
    let (events, mut failures) = match events::read(&dir.join("mail.sqlite"), 20) {
        Ok(events) => (events, Vec::new()),
        Err(error) => (
            events::ProjectEvents::default(),
            vec![format!("events: {error}")],
        ),
    };
    let (rows, diagnostics) = collect_rows(root, &config, id.trim(), &events, now);
    let prefix = format!("{} ", config.name);
    failures.extend(diagnostics.into_iter().map(|diagnostic| {
        match diagnostic.strip_prefix(prefix.as_str()) {
            Some(rest) => rest.to_owned(),
            None => diagnostic,
        }
    }));
    if failures.is_empty() {
        Ok(rows)
    } else {
        Err(ProjectFailure {
            project: config.name.clone(),
            sources: failures,
        })
    }
}

/// Collect rows from every source, plus one raw diagnostic per source that
/// could not be read. Slot diagnostics still carry the project prefix; the
/// caller normalizes it before aggregating.
fn collect_rows(
    root: &Path,
    config: &crate::mailspace::MailspaceConfig,
    id: &str,
    events: &events::ProjectEvents,
    now: u64,
) -> (Vec<model::MonitorRow>, Vec<String>) {
    let mut rows = Vec::new();
    let (mut local, mut diagnostics) =
        slots::rows(&config.name, root, id, &config.pools, events, now);
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
        diagnostics.append(&mut pump_warnings);
    }
    (rows, diagnostics)
}

/// One project observation that failed: the display name plus one diagnostic
/// per source that could not be read, rendered as a single warning naming the
/// project once.
struct ProjectFailure {
    project: String,
    sources: Vec<String>,
}

impl ProjectFailure {
    fn one(project: &str, source: &str, error: impl std::fmt::Display) -> Self {
        Self {
            project: project.to_owned(),
            sources: vec![format!("{source}: {error}")],
        }
    }

    fn warning(self) -> String {
        format!("{}: {}", self.project, self.sources.join("; "))
    }
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
