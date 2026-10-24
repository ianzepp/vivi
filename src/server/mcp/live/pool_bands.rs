//! `pool_bands`: operator-only show, set and unset of one pool's band table
//! (`[pools.<name>.bands]` in the hosted project's `.vivi/mailspace.toml`),
//! as `vivi pool bands set|unset` and `vivi pool show` do on the CLI.
//!
//! A band (`P0` to `P5`) names the model and reasoning effort that serves it.
//! `set` and `unset` go through the library's `config_edit::set_band` and
//! `unset_band` under `config_edit::guarded`, which edit only
//! `pools.<pool>.bands.<band>` (format-preserving, backup first, refused
//! while the pump runs, rolled back when the write makes the config worse).
//! The allow-list check is the library's too:
//! `vivi::pool::config_allow::validate_allow`.
//!
//! This is the one pool setting the server edits, and the reason it is safe
//! to expose: bands select among models the host already allowed. A model
//! must be listed in that pool's `harness.models`, so a band can never name
//! anything the host did not offer, and the tool reaches no other key: not a
//! pool's `remote_url` or `harness.run` (which would send code or commands
//! elsewhere), not another pool, not a role, not free-form config. Pools
//! themselves stay with the host.
//!
//! Authorization is [`super::pump`]'s, shared with the pump tools: the
//! operator's API key, minted by the host's own member, acting as a role of
//! kind `operator`. `show` is held to the same rule as `set`.

use std::path::Path;

use serde_json::{Map, Value, json};
use vivi::mailspace::{Mailspace, config_edit};
use vivi::pool::PoolConfig;
use vivi::pool::config_allow::harness_models;
use vivi::pool::config_allow::validate_allow;
use vivi::pool::config_bands::{BAND_KEYS, BandChoice};

use crate::identities::IdentityRecord;
use crate::mcp::tools::{ToolError, opt_str, rate_limit, req_str};
use crate::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};

/// The reasoning effort `set` writes when none is given, as the CLI does.
const DEFAULT_REASONING: &str = "xhigh";

/// Run `pool_bands` for `caller` acting as `acting`. Every action answers the
/// pool's table as it stands afterwards; `set` and `unset` are rate limited
/// as writes.
pub(in crate::mcp) fn pool_bands(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("pool_bands", "edit pool bands", acting)?;
    let action = req_str(arguments, "action")?;
    if !matches!(action, "show" | "set" | "unset") {
        return Err(ToolError::new(format!(
            "action must be show, set, or unset; got '{action}'"
        )));
    }
    let pool = req_str(arguments, "pool")?;
    let hosted = authorize("pool_bands", root, caller, requested)?;
    let config = pool_config(&hosted.ms, pool)?;
    if action != "show" {
        let band = band_key(req_str(arguments, "band")?)?;
        if action == "set" {
            let model = req_str(arguments, "model")?;
            let reasoning = opt_str(arguments, "reasoning")?.unwrap_or(DEFAULT_REASONING);
            check_set(config, pool, band, model, reasoning)?;
            rate_limit(limiter, project_id, &hosted.actor)?;
            config_edit::guarded(root, |doc| {
                config_edit::set_band(doc, pool, band, model, reasoning)
            })
            .map_err(lib_error)?;
        } else {
            rate_limit(limiter, project_id, &hosted.actor)?;
            config_edit::guarded(root, |doc| config_edit::unset_band(doc, pool, band))
                .map_err(lib_error)?;
        }
    }
    // A write changed the file: answer from a fresh read of it.
    let ms = Mailspace::open_root(root.to_path_buf()).map_err(lib_error)?;
    Ok(table(pool, pool_config(&ms, pool)?))
}

/// The pool `name` of the mailspace, or an error naming the pools it has.
fn pool_config<'a>(ms: &'a Mailspace, name: &str) -> Result<&'a PoolConfig, ToolError> {
    ms.config.pools.get(name).ok_or_else(|| {
        let names: Vec<&str> = ms.config.pools.keys().map(String::as_str).collect();
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

/// The canonical band key (`P0` to `P5`) for `text`, case-insensitive.
fn band_key(text: &str) -> Result<&'static str, ToolError> {
    BAND_KEYS
        .iter()
        .find(|key| key.eq_ignore_ascii_case(text))
        .copied()
        .ok_or_else(|| {
            ToolError::new(format!(
                "band must be one of {}; got '{text}'",
                BAND_KEYS.join(", ")
            ))
        })
}

/// Refuse a `set` the library would store but the host never allowed: a model
/// outside the pool's `harness.models` (`config_allow::validate_allow`, the
/// library's own check), or a malformed reasoning (checked by the library's
/// own band validation on the resulting pool).
fn check_set(
    config: &PoolConfig,
    pool: &str,
    band: &str,
    model: &str,
    reasoning: &str,
) -> Result<(), ToolError> {
    validate_allow(pool, config, model).map_err(lib_error)?;
    let mut next = config.clone();
    next.bands.entries.insert(
        band.to_owned(),
        vec![BandChoice {
            model: model.to_owned(),
            reasoning: reasoning.to_owned(),
            unknown: Default::default(),
        }],
    );
    next.check_bands(pool).map_err(lib_error)
}

/// `{pool, bands: {band: [{model, reasoning}]}, harness_models}`.
fn table(pool: &str, config: &PoolConfig) -> Value {
    let mut bands = Map::new();
    for (key, choices) in &config.bands.entries {
        let rows: Vec<Value> = choices
            .iter()
            .map(|choice| json!({"model": choice.model, "reasoning": choice.reasoning}))
            .collect();
        bands.insert(key.clone(), Value::Array(rows));
    }
    json!({
        "pool": pool,
        "bands": bands,
        "harness_models": harness_models(config),
    })
}
