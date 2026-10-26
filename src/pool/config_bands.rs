//! The intelligence bands a pool serves: the `[pools.<name>.bands]` table.
//!
//! A band is a model at an effort, never a model alone. The table maps each
//! band key `P0` to `P5` (`P0` the most capable) to an ordered list of
//! interchangeable weighted model-and-reasoning choices; an optional positive
//! `weight` (default one) sizes each choice's share. See [`super::bands`] for
//! the pure choice.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::{PoolConfig, PoolHarness};
use crate::error::VivariumError;

/// The band keys a `bands` table may hold, most capable first.
pub const BAND_KEYS: [&str; 6] = ["P0", "P1", "P2", "P3", "P4", "P5"];

/// The most choices one band's list may hold. Choices are interchangeable
/// weighted entries, so a longer list is refused by name.
pub const MAX_CHOICES_PER_BAND: usize = 4;

/// One model-and-reasoning choice a pool offers for a band. Choices are
/// interchangeable; an optional positive `weight` (default one) sizes each
/// choice's share. An explicit `weight` stays in [`Self::unknown`] so
/// serialization preserves it exactly as written, and an omitted weight stays
/// omitted.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BandChoice {
    /// Model slug the harness is run with.
    pub model: String,
    /// Reasoning effort the harness is run at, e.g. `low` or `xhigh`.
    pub reasoning: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl BandChoice {
    /// The choice's additive weight: the positive finite number it stores, or
    /// one when it stores no `weight`.
    ///
    /// # Errors
    /// Returns a configuration error when the choice stores a `weight` that
    /// is not a positive finite number.
    pub fn weight(&self) -> Result<f64, VivariumError> {
        checked_weight(self.unknown.get("weight")).map_err(|reason| {
            super::config::bad(format!(
                "band choice for model \"{}\" holds an invalid \"weight\": {reason}",
                self.model
            ))
        })
    }
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
    /// at most [`MAX_CHOICES_PER_BAND`] interchangeable choices, every
    /// `reasoning` is a short effort word, every `weight` a positive finite
    /// number, and every `model` is one the harness offers when a harness is
    /// configured. A pool with no `bands` table passes untouched.
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
            "bands.{key} holds {} choices; at most {MAX_CHOICES_PER_BAND} are allowed (choices are interchangeable weighted entries)",
            choices.len()
        )));
    }
    for choice in choices {
        reject_unrelated(pool, key, choice)?;
        check_weight(key, choice)?;
        check_reasoning(key, &choice.reasoning)?;
    }
    Ok(())
}

/// Refuse every unknown field a choice holds except `weight`, which
/// [`check_weight`] validates on its own.
fn reject_unrelated(pool: &str, key: &str, choice: &BandChoice) -> Result<(), VivariumError> {
    let unrelated: BTreeMap<String, toml::Value> = choice
        .unknown
        .iter()
        .filter(|(name, _)| name.as_str() != "weight")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    super::config::reject_unknown(pool, &format!(".bands.{key}"), &unrelated)
}

/// Refuse a stored `weight` that is not a positive finite number, naming the
/// band and the model that stores it. The same rule backs
/// [`BandChoice::weight`].
fn check_weight(key: &str, choice: &BandChoice) -> Result<(), VivariumError> {
    let Some(value) = choice.unknown.get("weight") else {
        return Ok(());
    };
    if let Err(reason) = checked_weight(Some(value)) {
        return Err(super::config::bad(format!(
            "bands.{key} gives model \"{}\" an invalid \"weight\": {reason}",
            choice.model
        )));
    }
    Ok(())
}

/// The additive weight a choice carries: one when it stores no `weight`, else
/// the positive finite number it stores. Anything else answers the reason it
/// is refused.
fn checked_weight(weight: Option<&toml::Value>) -> Result<f64, &'static str> {
    match weight {
        None => Ok(1.0),
        Some(toml::Value::Integer(n)) if *n > 0 => Ok(*n as f64),
        Some(toml::Value::Float(f)) if f.is_finite() && *f > 0.0 => Ok(*f),
        Some(_) => Err("weight must be a positive finite TOML integer or float"),
    }
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
