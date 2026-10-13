//! The intelligence bands a pool serves: the `[pools.<name>.bands]` table.
//!
//! A band is a model at an effort, never a model alone. The table maps each
//! band key `P0` to `P5` (`P0` the most capable) to an ordered list of
//! model-and-reasoning choices; the first choice serves the band and later
//! entries are reserved. See [`super::bands`] for the pure choice.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::{PoolConfig, PoolHarness};
use crate::error::VivariumError;

/// The band keys a `bands` table may hold, most capable first.
pub const BAND_KEYS: [&str; 6] = ["P0", "P1", "P2", "P3", "P4", "P5"];

/// The most choices one band's list may hold. The first serves the band and
/// later entries are reserved, so a longer list is refused by name.
pub const MAX_CHOICES_PER_BAND: usize = 4;

/// One model-and-reasoning choice a pool offers for a band.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BandChoice {
    /// Model slug the harness is run with.
    pub model: String,
    /// Reasoning effort the harness is run at, e.g. `low` or `xhigh`.
    pub reasoning: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// The `[pools.<name>.bands]` table: for each band key, the ordered
/// model-and-reasoning choices that serve it.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PoolBands {
    #[serde(flatten)]
    pub entries: BTreeMap<String, Vec<BandChoice>>,
}

impl PoolBands {
    /// Whether the pool sets no `bands` table at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl PoolConfig {
    /// Check the pool's `bands` table: keys are `P0` to `P5`, each band lists
    /// at most [`MAX_CHOICES_PER_BAND`] choices, every `reasoning` is a short
    /// effort word, and every `model` is one the harness offers when a
    /// harness is configured. A pool with no `bands` table passes untouched.
    ///
    /// # Errors
    /// Returns a configuration error naming the first bad key or choice.
    pub fn check_bands(&self, pool: &str) -> Result<(), VivariumError> {
        for (key, choices) in &self.bands.entries {
            check_key(key)?;
            check_choices(pool, key, choices)?;
        }
        if let Some(harness) = &self.harness {
            check_models(harness, &self.bands)?;
        }
        Ok(())
    }
}

fn check_key(key: &str) -> Result<(), VivariumError> {
    if BAND_KEYS.contains(&key) {
        return Ok(());
    }
    Err(super::config::bad(format!(
        "bands holds the key \"{key}\": a band key must be one of {}",
        BAND_KEYS.join(", ")
    )))
}

fn check_choices(pool: &str, key: &str, choices: &[BandChoice]) -> Result<(), VivariumError> {
    if choices.len() > MAX_CHOICES_PER_BAND {
        return Err(super::config::bad(format!(
            "bands.{key} holds {} choices; at most {MAX_CHOICES_PER_BAND} are allowed (the first serves the band, later entries are reserved)",
            choices.len()
        )));
    }
    for choice in choices {
        super::config::reject_unknown(pool, &format!(".bands.{key}"), &choice.unknown)?;
        check_reasoning(key, &choice.reasoning)?;
    }
    Ok(())
}

fn check_reasoning(key: &str, reasoning: &str) -> Result<(), VivariumError> {
    let fits = !reasoning.is_empty()
        && reasoning.chars().count() <= 32
        && reasoning
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if fits {
        return Ok(());
    }
    Err(super::config::bad(format!(
        "bands.{key} has reasoning \"{reasoning}\": use 1 to 32 characters of A-Z a-z 0-9 . _ -"
    )))
}

fn check_models(
    harness: &PoolHarness,
    bands: &super::config_bands::PoolBands,
) -> Result<(), VivariumError> {
    for (key, choices) in &bands.entries {
        for choice in choices {
            if !harness.models.contains(&choice.model) {
                return Err(super::config::bad(format!(
                    "bands.{key} gives model \"{}\", which harness.models does not offer ({})",
                    choice.model,
                    harness.models.join(", ")
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "config_bands_test.rs"]
mod tests;
