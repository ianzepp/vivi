//! The `pools.<name>.harness.models` allow-list: the check a caller runs
//! before it stores a model for a pool, and the guarded write of the list
//! itself.
//!
//! `validate_allow` is the check the hosted server's `pool_bands` tool makes
//! on every `set`: a model is allowed only when the pool's `harness.models`
//! names it, and a pool with no harness models allows nothing. `set_allow`
//! replaces the allow-list under the config guard, refusing a bad set before
//! anything reaches the file.

use std::path::Path;

use super::config::{PoolConfig, bad, is_model_slug};
use crate::error::VivariumError;
use crate::mailspace::config_edit::{self, GuardedWrite};
use crate::mailspace::{MAILSPACE_CONFIG, MAILSPACE_DIR};

/// The pool's `harness.models`; empty when the pool has no harness.
#[must_use]
pub fn harness_models(config: &PoolConfig) -> Vec<String> {
    config
        .harness
        .as_ref()
        .map_or_else(Vec::new, |harness| harness.models.clone())
}

/// Check `model` against `pool`'s `harness.models` allow-list, returning the
/// allowed set when the model is on it.
///
/// # Errors
/// Returns an error naming the allowed set — `(none: the pool has no harness
/// models)` when the list is empty or the pool has no harness — when `model`
/// is not on it.
pub fn validate_allow(
    pool: &str,
    config: &PoolConfig,
    model: &str,
) -> Result<Vec<String>, VivariumError> {
    let allowed = harness_models(config);
    if allowed.iter().any(|offered| offered == model) {
        return Ok(allowed);
    }
    Err(bad(format!(
        "model '{model}' is not in pool '{pool}' harness.models; allowed: {}",
        if allowed.is_empty() {
            "(none: the pool has no harness models)".to_owned()
        } else {
            allowed.join(", ")
        }
    )))
}

/// Replace the pool's allow-list (`pools.<pool>.harness.models`) with
/// `models`, under the config guard.
///
/// # Errors
/// Refuses the set before writing when a slug is not a model slug or the
/// list repeats one, and refuses an unknown pool. The guard may still roll
/// the write back when it leaves the configuration worse.
pub fn set_allow(
    root: &Path,
    pool: &str,
    models: &[String],
) -> Result<GuardedWrite, VivariumError> {
    check_set(pool, models)?;
    let file = root.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG);
    let doc = config_edit::load(&file)?;
    let pools = doc.get("pools").and_then(|item| item.as_table());
    if !pools.is_some_and(|pools| pools.contains_key(pool)) {
        return Err(bad(format!("unknown pool '{pool}'")));
    }
    let raw = toml_array(models);
    config_edit::guarded(root, |doc| {
        config_edit::set_pool_key(doc, pool, "harness.models", &raw)
    })
}

/// Refuse a set the harness validation would reject: a slug that is empty or
/// has characters outside the model-slug set, or a duplicate.
fn check_set(pool: &str, models: &[String]) -> Result<(), VivariumError> {
    if models.iter().any(|model| !is_model_slug(model)) {
        return Err(bad(format!(
            "pools.{pool}.harness.models holds an empty slug or one with characters outside A-Z a-z 0-9 . _ : / @ + -"
        )));
    }
    let mut seen = models.to_vec();
    seen.sort();
    seen.dedup();
    if seen.len() != models.len() {
        return Err(bad(format!(
            "pools.{pool}.harness.models holds a duplicate slug"
        )));
    }
    Ok(())
}

/// The set as a TOML string array. Slugs are checked first, so plain
/// quoting is safe.
fn toml_array(models: &[String]) -> String {
    let items: Vec<String> = models.iter().map(|model| format!("\"{model}\"")).collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
#[path = "config_allow_test.rs"]
mod tests;
