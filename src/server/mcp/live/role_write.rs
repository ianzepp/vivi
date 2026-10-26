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
//!
//! A Mind seat passes `role_set` through the operational guard
//! ([`super::pump::authorize_operational`]) but writes bindings only:
//! set-form `provider`, `pool` and `model` on an existing seat, from the
//! pools and providers the configuration already holds, and a model on
//! the effective pool's `harness.models` allow-list
//! ([`crate::pool::config_allow::validate_allow`]) — the host's offer,
//! never its growth. Forbidden or mixed fields, every clear form, and
//! role creation (an unknown seat) are refused before the rate limiter
//! and the guard run. An operator key keeps the full field set; every
//! other kind fails the guard, as does a key another member minted.
//! `charter_set` replaces one role's charter body through
//! `Mailspace::set_charter`, guarding the charter file the CLI guards,
//! with the body taken verbatim. The CLI's `@path`, `-` and `--body-file`
//! body forms are not accepted: the server takes the text.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::mailspace::config_edit;
use crate::mailspace::{Mailspace, RoleUpdate, RoleView};
use crate::pool::PoolConfig;
use crate::pool::config_allow::validate_allow;
use serde_json::{Value, json};

use crate::server::identities::IdentityRecord;
use crate::server::mcp::tools::{ToolError, opt_str, rate_limit, req_str};
use crate::server::safety::RateLimiter;

use super::config_write::open;
use super::lib_error;
use super::pump::{OperationalKind, acting_key, authorize_operational, operational_kind};
use super::string_list;

pub(in crate::server::mcp) fn role_add(
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

pub(in crate::server::mcp) fn role_set(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("role_set", "set a role", acting)?;
    let mut hosted = authorize_operational("role_set", root, caller, requested)?;
    let name = req_str(arguments, "role")?;
    let update = set_update(arguments)?;
    // A Mind seat passes the operational guard but writes bindings only;
    // an operator key keeps the full field set. The check runs before the
    // rate limiter and the guard, so a refusal writes nothing.
    let actor_kind = hosted.ms.role_view(&hosted.actor).map_err(lib_error)?.kind;
    if matches!(
        operational_kind(actor_kind.as_deref()),
        OperationalKind::Mind
    ) {
        mind_bindings(&hosted.ms, name, &update)?;
    }
    rate_limit(limiter, project_id, &hosted.actor)?;
    let config = hosted.ms.config_path();
    let (view, _) = config_edit::guard_file(root, &config, || hosted.ms.set_role(name, update))
        .map_err(lib_error)?;
    serde_json::to_value(&view)
        .map_err(|error| ToolError::new(format!("cannot encode the role: {error}")))
}

/// The Mind restriction on `role_set`: binding-only provider/pool/model
/// sets on an existing seat. Forbidden or mixed fields, every clear form,
/// and role creation are refused before anything is written; guard, rate
/// limiting, backups and audit attribution are the call's own either way.
fn mind_bindings(ms: &Mailspace, name: &str, update: &RoleUpdate) -> Result<(), ToolError> {
    let forbidden: Vec<&str> = [
        (update.kind.is_some(), "kind"),
        (update.status.is_some(), "status"),
        (update.harness.is_some(), "harness"),
        (update.thinking.is_some(), "thinking"),
        (update.cadence.is_some(), "cadence"),
        (update.read_only.is_some(), "read_only"),
        (update.models.is_some(), "models"),
        (!update.add_labels.is_empty(), "labels"),
        (!update.clear_labels.is_empty(), "clear_labels"),
        (matches!(update.provider, Some(None)), "clear_provider"),
        (matches!(update.model, Some(None)), "clear_model"),
        (matches!(update.pool, Some(None)), "clear_pool"),
    ]
    .into_iter()
    .filter(|(refused, _)| *refused)
    .map(|(_, field)| field)
    .collect();
    if !forbidden.is_empty() {
        return Err(ToolError::new(format!(
            "a mind seat may only set provider, pool and model on an existing role; refused: {}",
            forbidden.join(", ")
        )));
    }
    if update.provider.is_none() && update.pool.is_none() && update.model.is_none() {
        return Err(ToolError::new(
            "a mind seat may only set provider, pool and model; none was named",
        ));
    }
    // Role creation stays with the host: a mind seat rebinds an existing seat.
    let target = ms.role_view(name).map_err(|error| {
        ToolError::new(format!(
            "a mind seat rebinds an existing role only: {error}"
        ))
    })?;
    mind_binding_values(ms, &target, update)
}

/// The value checks behind [`mind_bindings`]: the pool and provider must
/// already be configured, and the model must sit on the effective pool's
/// `harness.models` allow-list — a mind seat uses the host's offer and
/// cannot grow it.
fn mind_binding_values(
    ms: &Mailspace,
    target: &RoleView,
    update: &RoleUpdate,
) -> Result<(), ToolError> {
    let pools = &ms.config.pools;
    if let Some(Some(pool)) = &update.pool {
        known_pool(pools, pool)?;
    }
    if let Some(Some(provider)) = &update.provider {
        let configured: BTreeSet<&str> =
            pools.values().map(|pool| pool.provider.as_str()).collect();
        if !configured.contains(provider.as_str()) {
            let names: Vec<&str> = configured.into_iter().collect();
            return Err(ToolError::new(format!(
                "provider '{provider}' is not a configured provider; configured: {}",
                if names.is_empty() {
                    "(none)".to_owned()
                } else {
                    names.join(", ")
                }
            )));
        }
    }
    if let Some(Some(model)) = &update.model {
        let named = match &update.pool {
            Some(Some(pool)) => Some(pool.clone()),
            _ => target.pool.clone(),
        };
        let Some(pool) = named else {
            return Err(ToolError::new(format!(
                "model '{model}' needs a pool: name one, or set the role's pool first"
            )));
        };
        let config = known_pool(pools, &pool)?;
        validate_allow(&pool, config, model).map_err(lib_error)?;
    }
    Ok(())
}

/// The configured pool `name`, or the pooled error `pool_bands` answers
/// with.
fn known_pool<'a>(
    pools: &'a BTreeMap<String, PoolConfig>,
    name: &str,
) -> Result<&'a PoolConfig, ToolError> {
    pools.get(name).ok_or_else(|| {
        let names: Vec<&str> = pools.keys().map(String::as_str).collect();
        ToolError::new(format!(
            "unknown pool '{name}'; the project's pools are: {}",
            if names.is_empty() {
                "(none)".to_owned()
            } else {
                names.join(", ")
            }
        ))
    })
}

pub(in crate::server::mcp) fn charter_set(
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
/// so unlike [`crate::server::mcp::tools::req_str`] this does not trim. A missing
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
