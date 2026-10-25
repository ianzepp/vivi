//! `[[identities]]` table edits.
#![allow(clippy::missing_errors_doc, clippy::option_option)]

use toml_edit::{DocumentMut, Table};

use super::{identities_mut, identity_index, set_opt_bool, set_opt_str, set_str_array};
use crate::error::VivariumError;

/// Fields a writer may put on one identity table. Absent slots are left
/// untouched; `Some(None)` clears the key.
#[derive(Debug, Clone, Default)]
#[allow(clippy::option_option)]
pub struct IdentityPatch {
    pub kind: Option<Option<String>>,
    pub status: Option<String>,
    pub harness: Option<Option<String>>,
    pub provider: Option<Option<String>>,
    pub model: Option<Option<String>>,
    pub models: Option<Vec<String>>,
    pub thinking: Option<Option<String>>,
    pub pid: Option<Option<u32>>,
    pub host: Option<Option<String>>,
    pub cadence: Option<Option<String>>,
    pub pool: Option<Option<String>>,
    pub read_only: Option<Option<bool>>,
    pub labels: Option<Vec<String>>,
    pub aliases: Option<Vec<String>>,
}

/// Append a new `[[identities]]` table with `name` and the keys in `patch`.
pub fn insert_identity(
    doc: &mut DocumentMut,
    name: &str,
    patch: &IdentityPatch,
) -> Result<(), VivariumError> {
    let identities = identities_mut(doc)?;
    if identity_index(identities, name).is_some() {
        return Err(VivariumError::Message(format!(
            "local role '{name}' already exists"
        )));
    }
    let mut table = Table::new();
    table["name"] = toml_edit::value(name);
    apply_patch(&mut table, patch);
    identities.push(table);
    Ok(())
}

/// Patch the identity named `name`.
pub fn patch_identity(
    doc: &mut DocumentMut,
    name: &str,
    patch: &IdentityPatch,
) -> Result<(), VivariumError> {
    let identities = identities_mut(doc)?;
    let index = identity_index(identities, name)
        .ok_or_else(|| VivariumError::Message(format!("unknown local role '{name}'")))?;
    let table = identities
        .get_mut(index)
        .ok_or_else(|| VivariumError::Message(format!("unknown local role '{name}'")))?;
    apply_patch(table, patch);
    Ok(())
}

/// Whether this file itself defines the identity `name` (as opposed to
/// inheriting it from a base).
#[must_use]
pub fn defines_identity(doc: &DocumentMut, name: &str) -> bool {
    doc.get("identities")
        .and_then(toml_edit::Item::as_array_of_tables)
        .is_some_and(|identities| super::identity_index(identities, name).is_some())
}

/// Patch the identity named `name` in this file, or, when this file does not
/// define it (the role comes from a base), append an override table holding
/// `name` and the patch's keys; the merge applies it key by key over the base
/// role. A clear cannot reach a base value from an override, so a patch that
/// clears a key of a role this file does not define is refused.
pub fn upsert_identity(
    doc: &mut DocumentMut,
    name: &str,
    patch: &IdentityPatch,
) -> Result<(), VivariumError> {
    if identity_index(identities_mut(doc)?, name).is_some() {
        return patch_identity(doc, name, patch);
    }
    if clears_a_key(patch) {
        return Err(VivariumError::Message(format!(
            "role '{name}' comes from a base config; clear an inherited key with \
             `unset = [\"identities.{name}.<key>\"]` in this project's file"
        )));
    }
    insert_identity(doc, name, patch)
}

fn clears_a_key(patch: &IdentityPatch) -> bool {
    [
        patch.kind.as_ref().is_some_and(Option::is_none),
        patch.harness.as_ref().is_some_and(Option::is_none),
        patch.provider.as_ref().is_some_and(Option::is_none),
        patch.model.as_ref().is_some_and(Option::is_none),
        patch.thinking.as_ref().is_some_and(Option::is_none),
        patch.pid.as_ref().is_some_and(Option::is_none),
        patch.host.as_ref().is_some_and(Option::is_none),
        patch.cadence.as_ref().is_some_and(Option::is_none),
        patch.pool.as_ref().is_some_and(Option::is_none),
        patch.read_only.as_ref().is_some_and(Option::is_none),
    ]
    .into_iter()
    .any(|cleared| cleared)
}

/// Rename an identity and record the old name as an alias.
pub fn rename_identity(doc: &mut DocumentMut, old: &str, new: &str) -> Result<(), VivariumError> {
    let identities = identities_mut(doc)?;
    if identity_index(identities, new).is_some() {
        return Err(VivariumError::Message(format!(
            "local role '{new}' already exists"
        )));
    }
    let index = identity_index(identities, old)
        .ok_or_else(|| VivariumError::Message(format!("unknown local role '{old}'")))?;
    let table = identities
        .get_mut(index)
        .ok_or_else(|| VivariumError::Message(format!("unknown local role '{old}'")))?;
    table["name"] = toml_edit::value(new);
    let mut aliases = string_array(table, "aliases");
    if !aliases.iter().any(|alias| alias == old) {
        aliases.push(old.to_string());
    }
    set_str_array(table, "aliases", &aliases);
    Ok(())
}

fn apply_patch(table: &mut Table, patch: &IdentityPatch) {
    apply_opt(table, "kind", patch.kind.as_ref());
    if let Some(status) = &patch.status {
        table["status"] = toml_edit::value(status.as_str());
    }
    apply_opt(table, "harness", patch.harness.as_ref());
    apply_opt(table, "provider", patch.provider.as_ref());
    apply_opt(table, "model", patch.model.as_ref());
    apply_opt(table, "thinking", patch.thinking.as_ref());
    apply_opt(table, "host", patch.host.as_ref());
    apply_opt(table, "cadence", patch.cadence.as_ref());
    apply_opt(table, "pool", patch.pool.as_ref());
    apply_pid(table, patch.pid);
    if let Some(flag) = patch.read_only {
        set_opt_bool(table, "read_only", flag);
    }
    if let Some(models) = &patch.models {
        set_str_array(table, "models", models);
    }
    if let Some(labels) = &patch.labels {
        set_str_array(table, "labels", labels);
    }
    if let Some(aliases) = &patch.aliases {
        set_str_array(table, "aliases", aliases);
    }
}

fn apply_opt(table: &mut Table, key: &str, value: Option<&Option<String>>) {
    let Some(value) = value else {
        return;
    };
    set_opt_str(table, key, value.as_deref());
}

fn apply_pid(table: &mut Table, pid: Option<Option<u32>>) {
    match pid {
        Some(Some(pid)) => table["pid"] = toml_edit::value(i64::from(pid)),
        Some(None) => {
            table.remove("pid");
        }
        None => {}
    }
}

fn string_array(table: &Table, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(|item| item.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(toml_edit::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
