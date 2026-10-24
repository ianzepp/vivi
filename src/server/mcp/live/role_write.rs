//! Role and charter writes on a hosted project: the same guarded library
//! calls the CLI's `vivi role add`, `vivi role set` and `vivi role
//! charter set` make, over the same arguments.
//!
//! `role_add` builds the CLI's full-field [`RoleUpdate`] — with the CLI's
//! default kind (the pump's hand role, `[pump] to`, `hand` with no
//! `[pump]` table, gets kind `hand` when `kind` is absent) — wraps
//! `Mailspace::add_role` in `config_edit::guard_file`, and then adopts
//! the letters held under the new name, as the CLI does.
//! `role_set` mirrors `vivi role set`'s configuration fields — the set
//! and clear forms of kind, harness, provider, model, thinking, cadence,
//! pool, read-only and models, plus status, labels and clear_labels — and
//! needs at least one of them, as the CLI does. The CLI's `--pid` and
//! `--host` are a live seat's runtime binding, the one `role set` write
//! that is not a guarded config write, and are not exposed here: every
//! server role write is a config write, refused while the pump runs.
//! `charter_set` replaces one role's charter body through
//! `Mailspace::set_charter`, guarding the charter file the CLI guards,
//! with the body taken verbatim. The CLI's `@path`, `-` and `--body-file`
//! body forms are not accepted: the server takes the text.

use std::path::Path;

use serde_json::{Value, json};
use vivi::mailspace::config_edit;
use vivi::mailspace::{Mailspace, RoleUpdate};

use crate::identities::IdentityRecord;
use crate::mcp::tools::{ToolError, opt_str, rate_limit, req_str};
use crate::safety::RateLimiter;

use super::config_write::open;
use super::lib_error;
use super::string_list;

pub(in crate::mcp) fn role_add(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut hosted = open("role_add", "add a role", root, caller, acting)?;
    let name = req_str(arguments, "role")?;
    let update = add_update(&hosted.ms, name, arguments)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    let config = hosted.ms.config_path();
    let (address, _) = config_edit::guard_file(root, &config, || hosted.ms.add_role(name, update))
        .map_err(lib_error)?;
    // The CLI's tail: letters held under the new name become its.
    let canonical = hosted.ms.resolve_identity(name).map_err(lib_error)?;
    let adopted = hosted
        .ms
        .adopt_held_letters(&canonical)
        .map_err(lib_error)?;
    let view = hosted.ms.role_view(&canonical).map_err(lib_error)?;
    Ok(json!({"added": address, "adopted": adopted, "role": view}))
}

pub(in crate::mcp) fn role_set(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut hosted = open("role_set", "set a role", root, caller, acting)?;
    let name = req_str(arguments, "role")?;
    let update = set_update(arguments)?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    let config = hosted.ms.config_path();
    let (view, _) = config_edit::guard_file(root, &config, || hosted.ms.set_role(name, update))
        .map_err(lib_error)?;
    serde_json::to_value(&view)
        .map_err(|error| ToolError::new(format!("cannot encode the role: {error}")))
}

pub(in crate::mcp) fn charter_set(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut hosted = open("charter_set", "set a charter", root, caller, acting)?;
    let name = req_str(arguments, "role")?;
    let charter = raw_body(arguments, "charter")?;
    rate_limit(limiter, project_id, &hosted.actor)?;
    let canonical = hosted.ms.resolve_identity(name).map_err(lib_error)?;
    let file = hosted.ms.charter_path(&canonical);
    config_edit::guard_file(root, &file, || hosted.ms.set_charter(name, charter))
        .map_err(lib_error)?;
    let body = hosted.ms.read_charter(&canonical).map_err(lib_error)?;
    Ok(json!({
        "name": canonical,
        "address": hosted.ms.address_for(&canonical),
        "has_charter": !body.is_empty(),
        "charter": body,
        "bytes": body.len(),
    }))
}

/// The `RoleUpdate` the CLI's `role add` builds: every field as given, no
/// clears, and its default kind for the pump's hand role.
fn add_update(ms: &Mailspace, name: &str, arguments: &Value) -> Result<RoleUpdate, ToolError> {
    let set = |field: &str| -> Result<Option<Option<String>>, ToolError> {
        Ok(opt_str(arguments, field)?.map(|value| Some(value.to_owned())))
    };
    let models = string_list(arguments, "models")?;
    Ok(RoleUpdate {
        kind: opt_str(arguments, "kind")?
            .or(default_add_kind(ms, name))
            .map(|kind| Some(kind.to_owned())),
        status: opt_str(arguments, "status")?.map(str::to_owned),
        harness: set("harness")?,
        provider: set("provider")?,
        model: set("model")?,
        thinking: set("thinking")?,
        cadence: set("cadence")?,
        add_labels: string_list(arguments, "labels")?,
        pool: set("pool")?,
        read_only: opt_bool(arguments, "read_only")?.map(Some),
        models: (!models.is_empty()).then_some(models),
        ..RoleUpdate::default()
    })
}

/// The kind a `role add` gives when `kind` is absent (the CLI's rule): the
/// pump's hand role — `[pump] to`, `hand` with no `[pump]` table — is a
/// hand; every other role gets none.
fn default_add_kind<'a>(ms: &'a Mailspace, name: &str) -> Option<&'a str> {
    let hand = ms
        .config
        .pump
        .as_ref()
        .map_or("hand", |pump| pump.to.as_str());
    (name == hand).then_some("hand")
}

/// The `RoleUpdate` the CLI's `role set` builds from its configuration
/// fields, refusing when no field is named (the CLI's rule).
fn set_update(arguments: &Value) -> Result<RoleUpdate, ToolError> {
    let mut update = RoleUpdate {
        add_labels: string_list(arguments, "labels")?,
        clear_labels: string_list(arguments, "clear_labels")?,
        ..RoleUpdate::default()
    };
    optional_field(&mut update.kind, arguments, "kind")?;
    if let Some(status) = opt_str(arguments, "status")? {
        update.status = Some(status.to_owned());
    }
    for (slot, field) in [
        (&mut update.harness, "harness"),
        (&mut update.provider, "provider"),
        (&mut update.model, "model"),
        (&mut update.thinking, "thinking"),
        (&mut update.cadence, "cadence"),
        (&mut update.pool, "pool"),
    ] {
        optional_field(slot, arguments, field)?;
    }
    if flag(arguments, "clear_read_only")? {
        update.read_only = Some(None);
    } else if let Some(value) = opt_bool(arguments, "read_only")? {
        update.read_only = Some(Some(value));
    }
    let models = string_list(arguments, "models")?;
    if flag(arguments, "clear_models")? {
        update.models = Some(Vec::new());
    } else if !models.is_empty() {
        update.models = Some(models);
    }
    if !update_has_fields(&update) {
        return Err(ToolError::new(
            "role_set requires at least one field to change",
        ));
    }
    Ok(update)
}

/// One set/clear pair: `clear_<field>` empties the field, else `<field>`
/// sets it.
#[allow(clippy::option_option)]
fn optional_field(
    slot: &mut Option<Option<String>>,
    arguments: &Value,
    field: &str,
) -> Result<(), ToolError> {
    if flag(arguments, &format!("clear_{field}"))? {
        *slot = Some(None);
    } else if let Some(value) = opt_str(arguments, field)? {
        *slot = Some(Some(value.to_owned()));
    }
    Ok(())
}

/// Whether a `role set` names anything to change (the CLI's rule, minus
/// pid and host, which the server does not expose).
fn update_has_fields(update: &RoleUpdate) -> bool {
    update.kind.is_some()
        || update.status.is_some()
        || update.harness.is_some()
        || update.provider.is_some()
        || update.model.is_some()
        || update.thinking.is_some()
        || update.cadence.is_some()
        || update.pool.is_some()
        || update.read_only.is_some()
        || update.models.is_some()
        || !update.add_labels.is_empty()
        || !update.clear_labels.is_empty()
}

fn opt_bool(arguments: &Value, name: &str) -> Result<Option<bool>, ToolError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ToolError::new(format!("{name} must be true or false"))),
    }
}

/// The charter body, exactly as given: the CLI writes the body verbatim,
/// so unlike [`crate::mcp::tools::req_str`] this does not trim. A missing
/// or blank body is refused.
fn raw_body<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|body| !body.trim().is_empty())
        .ok_or_else(|| ToolError::new(format!("{name} is required")))
}

fn flag(arguments: &Value, name: &str) -> Result<bool, ToolError> {
    Ok(opt_bool(arguments, name)?.unwrap_or(false))
}
