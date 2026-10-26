//! Operator config writes on a hosted project: the same library calls the
//! CLI makes, every one of them a guarded write.
//!
//! `config_migrate` with `write` wraps `pump::config::migrate` in
//! `config_edit::guard_file` (the library function itself is not guarded).
//! A dry run calls `migrate` with `write` false and is a read.
//! `config_set` / `config_unset` are `config_edit::set_key` / `unset_key`.
//!
//! One Mind-only exception in `config_set`: a `pools.<pool>.caps.<model>`
//! key may ride the operational guard, where a Mind seat's only write is an
//! integer reduction of that already-configured cap. Increases, new caps,
//! wrong types and every other key are refused before a write, an operator
//! keeps the full `set_key` behavior, and `config_unset`, `migrate` and the
//! allow-list and target tools stay operator-only.
//! `pool_allow_set` writes `[pools.<pool>.harness] models` — the allow list
//! `pool_bands` checks — through `crate::pool::config_allow::set_allow`
//! (check-then-write, guarded), after checking `models` is an array of
//! strings. `target_add` / `target_remove` are
//! `pump::config::targets::add` / `remove`.

use std::path::Path;

use toml_edit::{DocumentMut, Item};

use crate::error::VivariumError;
use crate::mailspace::config_edit;
use crate::pool::config_allow;
use crate::pump::config::targets::{self, TargetSpec};
use serde_json::{Value, json};

use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, opt_str, rate_limit, req_str};
use crate::server::safety::RateLimiter;

use super::pump::{
    OperationalKind, acting_key, authorize, authorize_operational, operational_kind,
};
use super::{Hosted, lib_error};

pub(in crate::server::mcp) fn config_migrate(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let hosted = open("config_migrate", "migrate config", root, caller, acting)?;
    let write = flag(arguments, "write")?;
    if !write {
        let report = crate::pump::config::migrate(Some(root), false).map_err(lib_error)?;
        return Ok(migrate_json(&report));
    }
    rate_limit(limiter, project_id, &hosted.actor)?;
    migrate_guarded(root)
}

pub(in crate::server::mcp) fn config_set(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let key = req_str(arguments, "key")?;
    let value = req_str(arguments, "value")?;
    // A `pools.<pool>.caps.<model>` key may take the operational guard;
    // every other key keeps the operator-only rule.
    let cap = cap_key(key);
    let requested = acting_key("config_set", "set config", acting)?;
    let hosted = if cap.is_some() {
        authorize_operational("config_set", root, caller, requested)?
    } else {
        authorize("config_set", root, caller, requested)?
    };
    rate_limit(limiter, project_id, &hosted.actor)?;
    // A Mind seat's one write is the guarded reduction; an operator falls
    // through to the full `set_key`, unchanged.
    if let Some(cap) = cap
        && seat_kind(&hosted)? == OperationalKind::Mind
    {
        return reduce_cap(root, key, value, arguments, &cap, &hosted);
    }
    let set = config_edit::set_key(root, key, value).map_err(lib_error)?;
    Ok(json!({"key": key, "value": set.value, "kind": set.kind}))
}

/// The cap a key names: exactly `pools.<pool>.caps.<model>`, parsed with
/// `config_edit`'s key parser so quoted segments stay whole (a model with
/// dots or slashes is one segment). Any other shape is not a cap.
struct CapKey {
    pool: String,
    model: String,
    path: Vec<String>,
}

fn cap_key(key: &str) -> Option<CapKey> {
    let Ok(path) = config_edit::parse_key(key) else {
        return None;
    };
    let [top, pool, caps, model] = path.as_slice() else {
        return None;
    };
    if top != "pools" || caps != "caps" || pool.is_empty() || model.is_empty() {
        return None;
    }
    Some(CapKey {
        pool: pool.clone(),
        model: model.clone(),
        path,
    })
}

/// The operational kind of the role this call acts as.
fn seat_kind(hosted: &Hosted) -> Result<OperationalKind, ToolError> {
    let kind = hosted.ms.role_view(&hosted.actor).map_err(lib_error)?.kind;
    Ok(operational_kind(kind.as_deref()))
}

/// The Mind seat's one write: reduce an already-configured
/// `pools.<pool>.caps.<model>` integer to a lower one, under the guard.
/// Only `key` and `value` may ride along, the value must be an integer a
/// caps entry can hold, and the reduction is checked against the current
/// configuration inside the guarded edit (running refusal, backup and
/// rollback all stay).
fn reduce_cap(
    root: &Path,
    key: &str,
    raw: &str,
    arguments: &Value,
    cap: &CapKey,
    hosted: &Hosted,
) -> Result<Value, ToolError> {
    only_key_value(arguments)?;
    let new = cap_value(raw)?;
    let file = crate::mailspace::Mailspace::open_root(root.to_path_buf())
        .map_err(lib_error)?
        .config_path();
    // Refuse before the guard touches anything when the live file already
    // fails the check; the guarded edit re-checks inside its own lock.
    let live = config_edit::load(&file).map_err(lib_error)?;
    check_reduction(&live, cap, new).map_err(lib_error)?;
    config_edit::guard_file(root, &file, || {
        config_edit::apply(&file, |doc| apply_reduction(doc, cap, new))
    })
    .map_err(lib_error)?;
    Ok(json!({
        "key": key,
        "value": new.to_string(),
        "kind": "integer",
        "by": hosted.actor,
    }))
}

/// The current integer value of the cap named by `cap`, walking
/// `pools` -> pool -> `caps` -> model in the document. Absent pools, absent
/// `caps` tables, absent models and non-integer values are all refused.
fn current_cap(doc: &DocumentMut, cap: &CapKey) -> Result<i64, VivariumError> {
    let name = cap.path.join(".");
    let not_set = || VivariumError::Config(format!("`{name}` is not set"));
    let pools = doc
        .get("pools")
        .and_then(Item::as_table_like)
        .ok_or_else(not_set)?;
    let pool = pools
        .get(&cap.pool)
        .and_then(Item::as_table_like)
        .ok_or_else(not_set)?;
    let caps = pool
        .get("caps")
        .and_then(Item::as_table_like)
        .ok_or_else(not_set)?;
    caps.get(&cap.model)
        .and_then(Item::as_value)
        .and_then(toml_edit::Value::as_integer)
        .ok_or_else(|| VivariumError::Config(format!("`{name}` is not a configured integer cap")))
}

/// The reduction rule against the current configuration: the new value must
/// be strictly lower than the cap that is set.
fn check_reduction(doc: &DocumentMut, cap: &CapKey, new: i64) -> Result<(), VivariumError> {
    let name = cap.path.join(".");
    let current = current_cap(doc, cap)?;
    if new >= current {
        return Err(VivariumError::Config(format!(
            "`{name}` may only be reduced: it is {current} now"
        )));
    }
    Ok(())
}

/// The guarded edit: re-check against the file as the guard holds it, then
/// lower the cap.
fn apply_reduction(doc: &mut DocumentMut, cap: &CapKey, new: i64) -> Result<(), VivariumError> {
    check_reduction(doc, cap, new)?;
    config_edit::set_in(doc, &cap.path, toml_edit::Value::from(new))
}

/// The TOML text of `value` must be an integer a caps entry can hold;
/// `PoolConfig` stores `BTreeMap<String, u32>`.
fn cap_value(raw: &str) -> Result<i64, ToolError> {
    let parsed = config_edit::parse_value(raw);
    let Some(value) = parsed.as_integer() else {
        return Err(ToolError::new(format!(
            "a cap must be an integer; `{raw}` is a {}",
            config_edit::value_type(&parsed)
        )));
    };
    if u32::try_from(value).is_err() {
        return Err(ToolError::new(format!(
            "a cap must be between 0 and {}: got {value}",
            u32::MAX
        )));
    }
    Ok(value)
}

/// The restricted path takes exactly `key` and `value`; any other argument
/// is refused before a write.
fn only_key_value(arguments: &Value) -> Result<(), ToolError> {
    let extra: Vec<String> = arguments
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, _)| name.clone())
        .filter(|name| name != "key" && name != "value")
        .collect();
    if extra.is_empty() {
        return Ok(());
    }
    Err(ToolError::new(format!(
        "reducing a cap takes only `key` and `value`; got {}",
        extra.join(", ")
    )))
}

pub(in crate::server::mcp) fn config_unset(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let hosted = open("config_unset", "unset config", root, caller, acting)?;
    let key = req_str(arguments, "key")?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    config_edit::unset_key(root, key).map_err(lib_error)?;
    Ok(json!({"key": key, "unset": true}))
}

pub(in crate::server::mcp) fn pool_allow_set(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let hosted = open(
        "pool_allow_set",
        "set a pool allow list",
        root,
        caller,
        acting,
    )?;
    let pool = req_str(arguments, "pool")?;
    let models = string_array(arguments, "models")?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    config_allow::set_allow(root, pool, &models).map_err(lib_error)?;
    Ok(json!({
        "pool": pool,
        "key": format!("pools.{pool}.harness.models"),
        "models": models,
    }))
}

pub(in crate::server::mcp) fn target_add(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let hosted = open("target_add", "add a pump target", root, caller, acting)?;
    let spec = target_spec(arguments)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    targets::add(root, &spec).map_err(lib_error)?;
    target_row(root, &spec.id)
}

pub(in crate::server::mcp) fn target_remove(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let hosted = open(
        "target_remove",
        "remove a pump target",
        root,
        caller,
        acting,
    )?;
    let id = req_str(arguments, "id")?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    targets::remove(root, id).map_err(lib_error)?;
    Ok(json!({"id": id, "removed": true}))
}

pub(super) fn open<'a>(
    tool: &str,
    purpose: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&'a str>,
) -> Result<super::Hosted, ToolError> {
    let requested = acting_key(tool, purpose, acting)?;
    authorize(tool, root, caller, requested)
}

/// `migrate(write: true)` under the guard: refused while the pump runs,
/// backup first, and a write that adds a new config error is rolled back.
fn migrate_guarded(root: &Path) -> Result<Value, ToolError> {
    let file = crate::mailspace::Mailspace::open_root(root.to_path_buf())
        .map_err(lib_error)?
        .config_path();
    let (report, _) = config_edit::guard_file(root, &file, || {
        crate::pump::config::migrate(Some(root), true)
    })
    .map_err(lib_error)?;
    Ok(migrate_json(&report))
}

fn migrate_json(report: &crate::pump::config::MigrateReport) -> Value {
    let charters: Vec<Value> = report
        .charters
        .iter()
        .map(|charter| {
            json!({
                "role": charter.role,
                "origin": charter.origin,
                "bytes": charter.bytes,
            })
        })
        .collect();
    json!({
        "plan": report.plan,
        "diff": report.diff,
        "dry_run": report.dry_run,
        "wrote": report.wrote.as_ref().map(|path| path.display().to_string()),
        "charters": charters,
    })
}

fn target_spec(arguments: &Value) -> Result<TargetSpec, ToolError> {
    Ok(TargetSpec {
        id: req_str(arguments, "id")?.to_owned(),
        path: req_str(arguments, "path")?.to_owned(),
        remote_url: owned(arguments, "remote_url")?,
        remote: owned(arguments, "remote")?,
        main: owned(arguments, "main")?,
        github_repo: owned(arguments, "github_repo")?,
        rungs: pairs(arguments, "rungs")?,
        merge: pairs(arguments, "merge")?,
    })
}

fn target_row(root: &Path, id: &str) -> Result<Value, ToolError> {
    let row = targets::list(root)
        .map_err(lib_error)?
        .into_iter()
        .find(|row| row.id == id)
        .ok_or_else(|| ToolError::new(format!("pump target `{id}` was not stored")))?;
    Ok(json!({
        "id": row.id,
        "path": row.path,
        "main": row.main,
        "rungs": row.rungs,
    }))
}

fn owned(arguments: &Value, name: &str) -> Result<Option<String>, ToolError> {
    Ok(opt_str(arguments, name)?.map(str::to_owned))
}

fn pairs(arguments: &Value, name: &str) -> Result<Vec<(String, String)>, ToolError> {
    let Some(value) = arguments.get(name) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = value.as_array() else {
        return Err(ToolError::new(format!(
            "{name} must be an array of NAME=VALUE strings"
        )));
    };
    items.iter().map(|item| one_pair(item, name)).collect()
}

fn one_pair(item: &Value, name: &str) -> Result<(String, String), ToolError> {
    let Some(text) = item.as_str() else {
        return Err(ToolError::new(format!(
            "{name} must be an array of NAME=VALUE strings"
        )));
    };
    targets::split_pair(text, name).map_err(lib_error)
}

fn flag(arguments: &Value, name: &str) -> Result<bool, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ToolError::new(format!("{name} must be true or false"))),
    }
}

/// `models` is a JSON array of strings. Anything else is refused before a write.
fn string_array(arguments: &Value, name: &str) -> Result<Vec<String>, ToolError> {
    let Some(Value::Array(items)) = arguments.get(name) else {
        return Err(ToolError::new(format!(
            "{name} must be an array of strings"
        )));
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| ToolError::new(format!("{name} must be an array of strings")))
        })
        .collect()
}
