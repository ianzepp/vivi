//! Generic `config set` / `config unset`: a dotted key path and a TOML-typed
//! value, written through the guard.
#![allow(clippy::missing_errors_doc)]

use std::path::Path;

use toml_edit::{DocumentMut, Item, Key, Table, TableLike, Value};

use super::{GuardedWrite, guarded};
use crate::error::VivariumError;

/// What `set_key` stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetKey {
    /// The TOML text of the stored value.
    pub value: String,
    /// The TOML type of the stored value.
    pub kind: &'static str,
    /// The guarded-write outcome.
    pub write: GuardedWrite,
}

/// Split a dotted key (quoted segments allowed) into its path.
pub fn parse_key(key: &str) -> Result<Vec<String>, VivariumError> {
    let parsed = Key::parse(key).map_err(|err| {
        VivariumError::Config(format!("`{key}` is not a valid config key: {err}"))
    })?;
    let path: Vec<String> = parsed.iter().map(|k| k.get().to_owned()).collect();
    if path.first().is_some_and(|first| first == "identities") {
        return Err(VivariumError::Config(
            "`identities` is the role roster: use vivi role add or vivi role set".into(),
        ));
    }
    Ok(path)
}

/// Read `raw` as a TOML value; a bare word that is not valid TOML is a string.
#[must_use]
pub fn parse_value(raw: &str) -> Value {
    raw.parse::<Value>().unwrap_or_else(|_| Value::from(raw))
}

/// The TOML type name of `value`.
#[must_use]
pub fn value_type(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "boolean",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::InlineTable(_) => "table",
    }
}

/// Set `path` to `value`, creating missing parent tables as implicit tables.
pub fn set_in(doc: &mut DocumentMut, path: &[String], value: Value) -> Result<(), VivariumError> {
    let (leaf, parents) = path
        .split_last()
        .ok_or_else(|| VivariumError::Config("empty config key".into()))?;
    let mut cur: &mut dyn TableLike = doc.as_table_mut();
    for (i, segment) in parents.iter().enumerate() {
        if !cur.contains_key(segment) {
            let mut table = Table::new();
            table.set_implicit(true);
            cur.insert(segment, Item::Table(table));
        }
        let dotted = path[..=i].join(".");
        let item = cur
            .get_mut(segment)
            .ok_or_else(|| VivariumError::Config(format!("`{dotted}` is not set")))?;
        if item.is_array_of_tables() {
            return Err(VivariumError::Config(format!(
                "`{dotted}` is an array of tables; edit it in mailspace.toml"
            )));
        }
        let kind = item.type_name();
        cur = item
            .as_table_like_mut()
            .ok_or_else(|| VivariumError::Config(format!("`{dotted}` is a {kind}, not a table")))?;
    }
    if cur
        .get(leaf)
        .is_some_and(|item| item.is_table() || item.is_array_of_tables())
    {
        return Err(VivariumError::Config(format!(
            "`{}` is a table; set a key inside it",
            path.join(".")
        )));
    }
    cur.insert(leaf, Item::Value(value));
    Ok(())
}

/// Remove `path`; a missing key is an error. Empty parents stay.
pub fn unset_in(doc: &mut DocumentMut, path: &[String]) -> Result<(), VivariumError> {
    let not_set = || VivariumError::Config(format!("`{}` is not set", path.join(".")));
    let (leaf, parents) = path.split_last().ok_or_else(not_set)?;
    let mut cur: &mut dyn TableLike = doc.as_table_mut();
    for segment in parents {
        cur = cur
            .get_mut(segment)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(not_set)?;
    }
    cur.remove(leaf).map(|_| ()).ok_or_else(not_set)
}

/// Set a dotted key to a TOML-typed value under the guard.
pub fn set_key(root: &Path, key: &str, raw: &str) -> Result<SetKey, VivariumError> {
    let path = parse_key(key)?;
    let value = parse_value(raw);
    let stored = value.to_string();
    let kind = value_type(&value);
    let write = guarded(root, |doc| set_in(doc, &path, value))?;
    Ok(SetKey {
        value: stored,
        kind,
        write,
    })
}

/// Remove a dotted key under the guard.
pub fn unset_key(root: &Path, key: &str) -> Result<GuardedWrite, VivariumError> {
    let path = parse_key(key)?;
    guarded(root, |doc| unset_in(doc, &path))
}

#[cfg(test)]
#[path = "keys_test.rs"]
mod tests;
