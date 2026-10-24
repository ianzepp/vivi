//! Pump target management: list, add and remove `[pump.targets.<id>]`.
//!
//! Library only: the caller prints. Writes go through the guarded config
//! edit, so a target that breaks the configuration is rolled back.
#![allow(clippy::missing_errors_doc)]

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, value};

use super::{PumpConfig, RUNG_NAMES};
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::mailspace::config_edit::{self, GuardedWrite};

/// Merge settings that `add` can write.
const MERGE_KEYS: [&str; 2] = ["format", "remote_limit"];

/// What the caller wants written for a new target.
#[derive(Debug, Clone, Default)]
pub struct TargetSpec {
    pub id: String,
    pub path: String,
    pub remote_url: Option<String>,
    pub remote: Option<String>,
    pub main: Option<String>,
    pub github_repo: Option<String>,
    /// `(rung name, command)` pairs.
    pub rungs: Vec<(String, String)>,
    /// `(merge key, raw value)` pairs; the value is read as TOML.
    pub merge: Vec<(String, String)>,
}

/// One configured target, for listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRow {
    pub id: String,
    pub path: String,
    pub main: String,
    /// Rung names set on the target itself.
    pub rungs: Vec<String>,
}

/// Split `NAME=VALUE` on the first `=`.
pub fn split_pair(text: &str, flag: &str) -> Result<(String, String), VivariumError> {
    match text.split_once('=') {
        Some((name, val)) if !name.trim().is_empty() => {
            Ok((name.trim().to_owned(), val.to_owned()))
        }
        _ => Err(VivariumError::Config(format!(
            "--{flag} wants NAME=VALUE, got `{text}`"
        ))),
    }
}

/// The configured targets in id order.
pub fn list(root: &Path) -> Result<Vec<TargetRow>, VivariumError> {
    let mailspace = Mailspace::discover(Some(root))?;
    let pump: PumpConfig = mailspace.config.pump.ok_or_else(no_pump)?;
    Ok(pump
        .targets
        .iter()
        .map(|(id, target)| TargetRow {
            id: id.clone(),
            path: target.path.clone(),
            main: target.main.clone(),
            rungs: RUNG_NAMES
                .iter()
                .filter(|name| target.rungs.command(name).is_some())
                .map(|name| (*name).to_owned())
                .collect(),
        })
        .collect())
}

/// Add `[pump.targets.<id>]` from `spec`.
pub fn add(root: &Path, spec: &TargetSpec) -> Result<GuardedWrite, VivariumError> {
    check_spec(spec)?;
    config_edit::guarded(root, |doc| {
        let targets = targets_table(doc)?;
        if targets.contains_key(&spec.id) {
            return Err(VivariumError::Config(format!(
                "pump target `{}` already exists",
                spec.id
            )));
        }
        targets.insert(&spec.id, Item::Table(target_table(spec)));
        Ok(())
    })
}

/// Remove `[pump.targets.<id>]`, and `pump.targets` when it ends up empty.
pub fn remove(root: &Path, id: &str) -> Result<GuardedWrite, VivariumError> {
    config_edit::guarded(root, |doc| {
        let pump = pump_table(doc)?;
        let targets = pump.get_mut("targets").and_then(Item::as_table_like_mut);
        let removed = targets.is_some_and(|t| t.remove(id).is_some());
        if !removed {
            return Err(VivariumError::Config(format!("unknown pump target `{id}`")));
        }
        let empty = pump
            .get("targets")
            .and_then(Item::as_table_like)
            .is_some_and(|t| t.is_empty());
        if empty {
            pump.remove("targets");
        }
        Ok(())
    })
}

fn no_pump() -> VivariumError {
    VivariumError::Config(String::from("no [pump] table in .vivi/mailspace.toml"))
}

fn check_spec(spec: &TargetSpec) -> Result<(), VivariumError> {
    for (name, command) in &spec.rungs {
        if !RUNG_NAMES.contains(&name.as_str()) {
            return Err(VivariumError::Config(format!(
                "unknown rung `{name}`; expected one of {}",
                RUNG_NAMES.join(", ")
            )));
        }
        if command.trim().is_empty() {
            return Err(VivariumError::Config(
                "the rung command must not be empty".into(),
            ));
        }
    }
    for (key, _) in &spec.merge {
        if key == "generated" {
            return Err(VivariumError::Config(format!(
                "edit [[pump.targets.{}.merge.generated]] in mailspace.toml",
                spec.id
            )));
        }
        if !MERGE_KEYS.contains(&key.as_str()) {
            return Err(VivariumError::Config(format!(
                "unknown merge setting `{key}`"
            )));
        }
    }
    Ok(())
}

fn pump_table(doc: &mut DocumentMut) -> Result<&mut Table, VivariumError> {
    doc.get_mut("pump")
        .and_then(Item::as_table_mut)
        .ok_or_else(no_pump)
}

fn targets_table(doc: &mut DocumentMut) -> Result<&mut Table, VivariumError> {
    let pump = pump_table(doc)?;
    if !pump.contains_key("targets") {
        let mut table = Table::new();
        table.set_implicit(true);
        pump.insert("targets", Item::Table(table));
    }
    pump.get_mut("targets")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| VivariumError::Config("`pump.targets` must be a table".into()))
}

fn target_table(spec: &TargetSpec) -> Table {
    let mut table = Table::new();
    table.insert("path", value(spec.path.as_str()));
    let optional = [
        ("github_repo", &spec.github_repo),
        ("remote_url", &spec.remote_url),
        ("remote", &spec.remote),
        ("main", &spec.main),
    ];
    for (key, val) in optional {
        if let Some(val) = val {
            table.insert(key, value(val.as_str()));
        }
    }
    if !spec.rungs.is_empty() {
        let mut rungs = Table::new();
        for (name, command) in &spec.rungs {
            rungs.insert(name, value(command.as_str()));
        }
        table.insert("rungs", Item::Table(rungs));
    }
    if !spec.merge.is_empty() {
        let mut merge = Table::new();
        for (key, raw) in &spec.merge {
            merge.insert(key, Item::Value(config_edit::parse_value(raw)));
        }
        table.insert("merge", Item::Table(merge));
    }
    table
}

#[cfg(test)]
#[path = "targets_test.rs"]
mod tests;
