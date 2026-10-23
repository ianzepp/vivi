//! Comment-preserving edits of `mailspace.toml`.
//!
//! Writers go through [`toml_edit`] so a `role set` or `pool add` never
//! rewrites the file from the in-memory struct (which would drop comments
//! and invent default keys the user never set).
#![allow(clippy::missing_errors_doc)]

use std::path::Path;

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value, value};

use super::config_file::{edit_document, parse_document, read_raw, write_raw};
use crate::error::VivariumError;

mod guard;
mod identity;
mod keys;
mod pool;

#[cfg(test)]
#[path = "config_edit_test.rs"]
mod tests;

pub use guard::{GuardedWrite, guard_file, guarded};
pub use identity::{IdentityPatch, insert_identity, patch_identity, rename_identity};
pub use keys::{SetKey, parse_key, parse_value, set_in, set_key, unset_in, unset_key, value_type};
pub use pool::{
    PoolAdd, remove_pool, set_band, set_pool_key, unset_band, unset_pool_key, upsert_pool,
};

/// Apply `edit` to the document at `path` and write it back when it changes.
pub fn apply(
    path: &Path,
    edit: impl FnOnce(&mut DocumentMut) -> Result<(), VivariumError>,
) -> Result<(), VivariumError> {
    edit_document(path, edit)
}

/// Parse `path` without writing; used by dry-run migrate.
pub fn load(path: &Path) -> Result<DocumentMut, VivariumError> {
    let raw = read_raw(path)?;
    parse_document(path, &raw)
}

/// Replace the file under the config lock.
pub fn write_text(path: &Path, raw: &str) -> Result<(), VivariumError> {
    write_raw(path, raw)
}

/// The `[[identities]]` array, created if missing.
///
/// Serde writes an empty roster as `identities = []`. Promote that to an
/// array of tables so the first `role add` can append `[[identities]]`.
pub(crate) fn identities_mut(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables, VivariumError> {
    if needs_identity_array(doc) {
        doc["identities"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    doc["identities"].as_array_of_tables_mut().ok_or_else(|| {
        VivariumError::Config("`identities` must be an array of tables (`[[identities]]`)".into())
    })
}

fn needs_identity_array(doc: &DocumentMut) -> bool {
    match doc.get("identities") {
        None => true,
        Some(item) => item.as_array().is_some_and(Array::is_empty),
    }
}

/// The identity table whose `name` matches, if any.
pub(crate) fn identity_index(identities: &ArrayOfTables, name: &str) -> Option<usize> {
    identities.iter().position(|table| name_is(table, name))
}

fn name_is(table: &Table, name: &str) -> bool {
    table
        .get("name")
        .and_then(Item::as_str)
        .is_some_and(|found| found == name)
}

/// Set or clear a string key on `table`. `None` removes the key.
pub(crate) fn set_opt_str(table: &mut Table, key: &str, value: Option<&str>) {
    match value {
        Some(text) => table[key] = self::value(text),
        None => {
            table.remove(key);
        }
    }
}

/// Set or clear a boolean key on `table`.
pub(crate) fn set_opt_bool(table: &mut Table, key: &str, value: Option<bool>) {
    match value {
        Some(flag) => table[key] = self::value(flag),
        None => {
            table.remove(key);
        }
    }
}

/// Replace `key` with a string array, or remove it when `items` is empty.
pub(crate) fn set_str_array(table: &mut Table, key: &str, items: &[String]) {
    if items.is_empty() {
        table.remove(key);
        return;
    }
    let mut array = Array::new();
    for item in items {
        array.push(item.as_str());
    }
    table[key] = Item::Value(Value::Array(array));
}

/// Walk a dotted key under `table`, creating intermediate tables as needed.
pub(crate) fn table_at<'a>(
    table: &'a mut Table,
    path: &[&str],
) -> Result<&'a mut Table, VivariumError> {
    if path.is_empty() {
        return Err(VivariumError::Config("empty config key path".into()));
    }
    let (leaf, parents) = path.split_last().unwrap_or((&"", &[]));
    let mut current = table;
    for part in parents {
        current = child_table(current, part)?;
    }
    child_table(current, leaf)
}

fn child_table<'a>(table: &'a mut Table, key: &str) -> Result<&'a mut Table, VivariumError> {
    table
        .entry(key)
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| {
            VivariumError::Config(format!("`{key}` must be a table for nested pool settings"))
        })
}

/// The `[pools]` table, created if missing.
pub(crate) fn pools_mut(doc: &mut DocumentMut) -> Result<&mut Table, VivariumError> {
    if doc.get("pools").is_none() {
        doc["pools"] = Item::Table(Table::new());
    }
    doc["pools"]
        .as_table_mut()
        .ok_or_else(|| VivariumError::Config("`pools` must be a table (`[pools.<name>]`)".into()))
}
