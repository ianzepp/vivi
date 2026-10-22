//! Table merging and `unset` for config inheritance.

use toml::{Table, Value};

use super::origins::{ConfigOrigins, Tracker};
use super::{Layer, is_inherited_key};
use crate::error::VivariumError;

/// Merge `layers` (farthest base first, the project last) into one table.
pub(super) fn merge_layers(
    layers: Vec<Layer>,
    skipped: Vec<(std::path::PathBuf, String)>,
) -> Result<(Table, ConfigOrigins), VivariumError> {
    let mut tracker = Tracker::new(layers.iter().map(|layer| layer.path.clone()).collect());
    let last = layers.len().saturating_sub(1);
    let mut merged = Table::new();
    for (index, layer) in layers.into_iter().enumerate() {
        let Layer { path, mut table } = layer;
        let unset = take_unset(&path, &mut table)?;
        if index != last {
            table.retain(|key, _| is_inherited_key(key));
        }
        for dotted in unset {
            let segments = split_path(&dotted);
            let matched = !segments.is_empty() && remove_path(&mut merged, &segments);
            tracker.unset(index, dotted, &segments, matched);
        }
        tracker.record(index, &table);
        merge_into(&mut merged, table, true);
    }
    Ok((merged, tracker.finish(skipped)))
}

/// Merge `child` over `base`: tables key by key, everything else replaced.
/// `top` marks the file's top level, where `identities` merges by role name.
fn merge_into(base: &mut Table, child: Table, top: bool) {
    for (key, value) in child {
        let named = top && key == "identities";
        let value = match base.get_mut(&key) {
            Some(existing) => match merge_value(existing, value, named) {
                Some(replacement) => replacement,
                None => continue,
            },
            None => value,
        };
        base.insert(key, value);
    }
}

/// Merge `child` into `existing` in place, or hand it back to replace it.
fn merge_value(existing: &mut Value, child: Value, named: bool) -> Option<Value> {
    match (existing, child) {
        (Value::Table(base), Value::Table(child)) => {
            merge_into(base, child, false);
            None
        }
        (Value::Array(base), Value::Array(child)) if named => {
            merge_roles(base, child);
            None
        }
        (_, child) => Some(child),
    }
}

/// Merge role tables by `name`; a role without a match is appended.
fn merge_roles(base: &mut Vec<Value>, child: Vec<Value>) {
    for role in child {
        let at = role_name(&role)
            .and_then(|name| base.iter().position(|known| role_name(known) == Some(name)));
        match (at, role) {
            (Some(at), Value::Table(role)) => {
                if let Value::Table(known) = &mut base[at] {
                    merge_into(known, role, false);
                }
            }
            (_, role) => base.push(role),
        }
    }
}

pub(super) fn role_name(role: &Value) -> Option<&str> {
    role.get("name").and_then(Value::as_str)
}

/// Remove and return the file's `unset` list.
fn take_unset(path: &std::path::Path, table: &mut Table) -> Result<Vec<String>, VivariumError> {
    let Some(value) = table.remove("unset") else {
        return Ok(Vec::new());
    };
    let bad = || {
        VivariumError::Config(format!(
            "`unset` in {} must be a list of dotted key paths",
            path.display()
        ))
    };
    let Value::Array(items) = value else {
        return Err(bad());
    };
    items
        .into_iter()
        .map(|item| item.try_into::<String>().map_err(|_| bad()))
        .collect()
}

/// Split a dotted path; a segment holding a dot is double-quoted.
pub(super) fn split_path(dotted: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in dotted.trim().chars() {
        match ch {
            '"' => quoted = !quoted,
            '.' if !quoted => segments.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    segments.push(current);
    if quoted || segments.iter().any(String::is_empty) {
        return Vec::new();
    }
    segments
}

/// Remove `segments` from `table`; true when something was removed.
fn remove_path(table: &mut Table, segments: &[String]) -> bool {
    let Some((first, rest)) = segments.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return table.remove(first).is_some();
    }
    match table.get_mut(first) {
        Some(Value::Table(inner)) => remove_path(inner, rest),
        Some(Value::Array(roles)) => remove_in_roles(roles, rest),
        _ => false,
    }
}

fn remove_in_roles(roles: &mut Vec<Value>, segments: &[String]) -> bool {
    let Some((name, rest)) = segments.split_first() else {
        return false;
    };
    let Some(at) = roles
        .iter()
        .position(|role| role_name(role) == Some(name.as_str()))
    else {
        return false;
    };
    if rest.is_empty() {
        roles.remove(at);
        return true;
    }
    match &mut roles[at] {
        Value::Table(role) => remove_path(role, rest),
        _ => false,
    }
}
