//! `[pools.<name>]` table edits.
#![allow(clippy::missing_errors_doc)]

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use super::{pools_mut, table_at};
use crate::error::VivariumError;
use crate::pool::config::check_pool_name;

/// Keys written by `vivi pool add`. Only fields the user set.
#[derive(Debug, Clone)]
pub struct PoolAdd {
    pub name: String,
    pub provider: String,
    pub slots: u32,
    pub worktree_of: Option<String>,
    pub repo: Option<String>,
    pub image: Option<String>,
    pub root: Option<String>,
}

/// Insert `[pools.<name>]` with the keys in `add`.
pub fn upsert_pool(doc: &mut DocumentMut, add: &PoolAdd) -> Result<(), VivariumError> {
    check_pool_name(&add.name)?;
    let pools = pools_mut(doc)?;
    if pools.get(&add.name).is_some_and(|item| !item.is_none()) {
        return Err(VivariumError::Message(format!(
            "pool '{}' already exists",
            add.name
        )));
    }
    let mut table = Table::new();
    table["provider"] = toml_edit::value(add.provider.as_str());
    table["slots"] = toml_edit::value(i64::from(add.slots));
    write_optional(&mut table, add);
    pools[&add.name] = Item::Table(table);
    Ok(())
}

fn write_optional(table: &mut Table, add: &PoolAdd) {
    if let Some(repo) = &add.repo {
        table["repo"] = toml_edit::value(repo.as_str());
    }
    if let Some(image) = &add.image {
        table["image"] = toml_edit::value(image.as_str());
    }
    if let Some(path) = &add.worktree_of {
        table["local"]["worktree_of"] = toml_edit::value(path.as_str());
    }
    if let Some(root) = &add.root {
        table["local"]["root"] = toml_edit::value(root.as_str());
    }
}

/// Set a (possibly dotted) key on `[pools.<name>]`.
pub fn set_pool_key(
    doc: &mut DocumentMut,
    name: &str,
    key: &str,
    raw: &str,
) -> Result<(), VivariumError> {
    let table = pool_table_mut(doc, name)?;
    let parts: Vec<&str> = key.split('.').filter(|part| !part.is_empty()).collect();
    let (leaf, parents) = parts
        .split_last()
        .ok_or_else(|| VivariumError::Message("pool key must not be empty".into()))?;
    let target = if parents.is_empty() {
        table
    } else {
        table_at(table, parents)?
    };
    target.insert(leaf, Item::Value(parse_value(raw)));
    Ok(())
}

/// Remove a (possibly dotted) key from `[pools.<name>]`.
pub fn unset_pool_key(doc: &mut DocumentMut, name: &str, key: &str) -> Result<(), VivariumError> {
    let table = pool_table_mut(doc, name)?;
    let parts: Vec<&str> = key.split('.').filter(|part| !part.is_empty()).collect();
    let (leaf, parents) = parts
        .split_last()
        .ok_or_else(|| VivariumError::Message("pool key must not be empty".into()))?;
    let target = if parents.is_empty() {
        table
    } else {
        table_at(table, parents)?
    };
    if target.remove(leaf).is_none() {
        return Err(VivariumError::Message(format!(
            "pool '{name}' has no key '{key}'"
        )));
    }
    Ok(())
}

/// Delete `[pools.<name>]`.
pub fn remove_pool(doc: &mut DocumentMut, name: &str) -> Result<(), VivariumError> {
    let pools = pools_mut(doc)?;
    if pools.remove(name).is_none() {
        return Err(VivariumError::Message(format!("unknown pool '{name}'")));
    }
    Ok(())
}

/// Replace `[pools.<name>.bands.<band>]` with one model/reasoning choice.
pub fn set_band(
    doc: &mut DocumentMut,
    pool: &str,
    band: &str,
    model: &str,
    reasoning: &str,
) -> Result<(), VivariumError> {
    let table = pool_table_mut(doc, pool)?;
    let mut choice = Table::new();
    choice["model"] = toml_edit::value(model);
    choice["reasoning"] = toml_edit::value(reasoning);
    let mut rows = ArrayOfTables::new();
    rows.push(choice);
    table["bands"][band] = Item::ArrayOfTables(rows);
    Ok(())
}

/// Remove `[pools.<name>.bands.<band>]`.
pub fn unset_band(doc: &mut DocumentMut, pool: &str, band: &str) -> Result<(), VivariumError> {
    let table = pool_table_mut(doc, pool)?;
    let Some(bands) = table["bands"].as_table_mut() else {
        return Err(VivariumError::Message(format!(
            "pool '{pool}' has no bands table"
        )));
    };
    if bands.remove(band).is_none() {
        return Err(VivariumError::Message(format!(
            "pool '{pool}' has no band '{band}'"
        )));
    }
    Ok(())
}

fn pool_table_mut<'a>(
    doc: &'a mut DocumentMut,
    name: &str,
) -> Result<&'a mut Table, VivariumError> {
    let pools = pools_mut(doc)?;
    pools
        .get_mut(name)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| VivariumError::Message(format!("unknown pool '{name}'")))
}

fn parse_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    if let Ok(number) = trimmed.parse::<i64>() {
        return number.into();
    }
    if trimmed == "true" {
        return true.into();
    }
    if trimmed == "false" {
        return false.into();
    }
    parse_array_or_string(trimmed)
}

fn parse_array_or_string(raw: &str) -> Value {
    if raw.starts_with('[')
        && let Ok(doc) = format!("v = {raw}").parse::<DocumentMut>()
        && let Some(value) = doc.get("v").and_then(Item::as_value)
    {
        return value.clone();
    }
    raw.into()
}
