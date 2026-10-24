//! Operator config writes on a hosted project: the same library calls the
//! CLI makes, every one of them a guarded write.
//!
//! `config_migrate` with `write` wraps `pump::config::migrate` in
//! `config_edit::guard_file` (the library function itself is not guarded).
//! A dry run calls `migrate` with `write` false and is a read.
//! `config_set` / `config_unset` are `config_edit::set_key` / `unset_key`.
//! `pool_allow_set` writes `[pools.<pool>.harness] models` — the allow list
//! `pool_bands` checks — through `vivi::pool::config_allow::set_allow`
//! (check-then-write, guarded), after checking `models` is an array of
//! strings. `target_add` / `target_remove` are
//! `pump::config::targets::add` / `remove`.

use std::path::Path;

use serde_json::{Value, json};
use vivi::mailspace::config_edit;
use vivi::pool::config_allow;
use vivi::pump::config::targets::{self, TargetSpec};

use crate::identities::IdentityRecord;
use crate::mcp::tools::{ToolError, opt_str, rate_limit, req_str};
use crate::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};

pub(in crate::mcp) fn config_migrate(
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
        let report = vivi::pump::config::migrate(Some(root), false).map_err(lib_error)?;
        return Ok(migrate_json(&report));
    }
    rate_limit(limiter, project_id, &hosted.actor)?;
    migrate_guarded(root)
}

pub(in crate::mcp) fn config_set(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let hosted = open("config_set", "set config", root, caller, acting)?;
    let key = req_str(arguments, "key")?;
    let value = req_str(arguments, "value")?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    let set = config_edit::set_key(root, key, value).map_err(lib_error)?;
    Ok(json!({"key": key, "value": set.value, "kind": set.kind}))
}

pub(in crate::mcp) fn config_unset(
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

pub(in crate::mcp) fn pool_allow_set(
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

pub(in crate::mcp) fn target_add(
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

pub(in crate::mcp) fn target_remove(
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
    let file = vivi::mailspace::Mailspace::open_root(root.to_path_buf())
        .map_err(lib_error)?
        .config_path();
    let (report, _) = config_edit::guard_file(root, &file, || {
        vivi::pump::config::migrate(Some(root), true)
    })
    .map_err(lib_error)?;
    Ok(migrate_json(&report))
}

fn migrate_json(report: &vivi::pump::config::MigrateReport) -> Value {
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
