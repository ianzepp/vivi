//! Read and set the pump gate command (`[pump.gate] command`) in the
//! project's own `.vivi/mailspace.toml`.
//!
//! The setter is a format-preserving edit through the shared mailspace config
//! writer, so it takes the same `mailspace.toml.lock` as `role` and `pool`
//! edits and leaves every other byte of the file as it was.
#![allow(clippy::missing_errors_doc)]

use std::path::{Path, PathBuf};

use toml_edit::{Item, Table, Value};

use crate::error::VivariumError;
use crate::mailspace::config_edit;
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR};

#[cfg(test)]
#[path = "gate_config_test.rs"]
mod tests;

/// `<project_root>/.vivi/mailspace.toml`.
fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG)
}

/// Set `[pump.gate] command` to `command`, creating `[pump]` and
/// `[pump.gate]` when missing. The command is stored as given, without
/// trimming.
///
/// Fails when the command is empty or whitespace-only, when the project has
/// no mailspace config, or when `pump` or `pump.gate` is not a table.
pub fn set_command(project_root: &Path, command: &str) -> Result<(), VivariumError> {
    if command.trim().is_empty() {
        return Err(VivariumError::Config(
            "the pump gate command must not be empty".into(),
        ));
    }
    config_edit::apply(&config_path(project_root), |doc| {
        let pump = table_entry(doc.as_table_mut(), "pump")?;
        let gate = table_entry(pump, "gate")?;
        replace_keeping_decor(gate, "command", command);
        Ok(())
    })
}

/// The `[pump.gate] command` text in the project's mailspace config, or
/// `None` when the key is not set or blank (the starter `[pump]` writes
/// `command = ""`, which readiness also treats as missing).
pub fn command(project_root: &Path) -> Result<Option<String>, VivariumError> {
    let doc = config_edit::load(&config_path(project_root))?;
    Ok(doc
        .get("pump")
        .and_then(|pump| pump.get("gate"))
        .and_then(|gate| gate.get("command"))
        .and_then(Item::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned))
}

/// Set `key` to a string, keeping the old value's surrounding whitespace and
/// trailing comment when the key already holds a value.
fn replace_keeping_decor(table: &mut Table, key: &str, text: &str) {
    let mut next = Value::from(text);
    if let Some(old) = table.get(key).and_then(Item::as_value) {
        *next.decor_mut() = old.decor().clone();
    }
    table[key] = Item::Value(next);
}

/// The child table `key` of `parent`, created (implicit, so it prints only
/// through its children) when missing.
fn table_entry<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table, VivariumError> {
    if !parent.contains_key(key) {
        let mut table = Table::new();
        table.set_implicit(true);
        parent.insert(key, Item::Table(table));
    }
    parent
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| VivariumError::Config(format!("`{key}` must be a table")))
}
