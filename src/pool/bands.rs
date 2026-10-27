//! Intelligence bands: parse a band string, read a task body's `band:` line,
//! and choose the model-and-reasoning pair a pool offers for a band.
//!
//! A band is a model at an effort, never a model alone. A role's `model`
//! field holds a band string such as `P2-S0`; a task body may carry a
//! `band:` line that overrides the role's band for one assignment. A pool's
//! `[pools.<name>.bands]` table (see [`super::config_bands`]) lists the
//! choices that serve each band, every listed entry taking a share of the
//! work; the `S` speed axis is parsed and ignored.

pub mod cooldown;
pub mod guard;
pub mod live;
pub mod runtime;

use rand_core::{OsRng, RngCore};

use super::config::PoolConfig;
use super::config_bands::{BandChoice, PoolBands};
use crate::error::VivariumError;

/// The most choices one band's list may hold. Mirrors the configuration cap
/// (see [`super::config_bands`]): the sampler refuses a longer list rather
/// than truncating it.
const MAX_CHOICES: usize = 4;

/// The speed suffixes a band string may carry. Parsed and ignored: the S
/// axis does not yet change which model-and-reasoning pair serves a band.
const SPEEDS: [&str; 4] = ["S0", "S1", "S2", "S3"];

/// The band of a band string: `P0` to `P5`, case-insensitive, optionally
/// followed by `-S0` to `-S3` (also case-insensitive). `P2-S0` is band 2;
/// anything else, including `P6` or `S1`, is no band at all.
#[must_use]
pub fn parse_band(text: &str) -> Option<u8> {
    match text.split_once('-') {
        Some((band, speed)) => {
            let band = band_digit(band)?;
            if !SPEEDS.iter().any(|s| speed.eq_ignore_ascii_case(s)) {
                return None;
            }
            Some(band)
        }
        None => band_digit(text),
    }
}

/// The `P0` to `P5` half of a band string, case-insensitive.
fn band_digit(text: &str) -> Option<u8> {
    match text.as_bytes() {
        [p, d] if p.eq_ignore_ascii_case(&b'P') && (b'0'..=b'5').contains(d) => Some(d - b'0'),
        _ => None,
    }
}

/// A model-and-reasoning pair a pool offers for a band: one of the band's
/// configured choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The band this choice serves, `0` to `5`.
    pub band: u8,
    /// Model slug the harness is run with.
    pub model: String,
    /// Reasoning effort the harness is run at.
    pub reasoning: String,
}

/// The model-and-reasoning pair `pool` offers for `band`: sampled by
/// relative weight over the complete list in the pool's
/// `[pools.<name>.bands]` table, so every listed entry takes a share of the
/// work and none is a primary with the rest as spares.
///
/// # Errors
/// Returns an error naming the band, the pool and the bands the pool does
/// serve when the pool has nothing for the band: send the job to a pool that
/// serves it, or pass `--model`. Returns an error too for a list the
/// configuration cap refuses, and for an entropy failure of the system
/// source of randomness.
pub fn choose(config: &PoolConfig, pool: &str, band: u8) -> Result<Choice, VivariumError> {
    let mut rng = OsRng;
    sample_pair(config, pool, band, &mut rng)
}

/// [`choose`] with the source of randomness injected: the seam [`runtime`]
/// and the tests share.
///
/// # Errors
/// As [`choose`].
pub(crate) fn sample_pair<R: RngCore>(
    config: &PoolConfig,
    pool: &str,
    band: u8,
    rng: &mut R,
) -> Result<Choice, VivariumError> {
    let key = format!("P{band}");
    let list = config
        .bands
        .entries
        .get(&key)
        .filter(|list| !list.is_empty())
        .ok_or_else(|| not_served(pool, band, &config.bands))?;
    let entry = sample(&key, list, rng)?;
    Ok(Choice {
        band,
        model: entry.model.clone(),
        reasoning: entry.reasoning.clone(),
    })
}

/// Weighted sampling over the complete list of choices a band configures:
/// every entry takes a share of the work sized by its `weight` (default 1),
/// drawn by rejection sampling against the weight total, so no entry is a
/// primary and none is a spare. A single entry returns its pair without
/// consulting the source of randomness at all.
///
/// # Errors
/// Returns a configuration error for a list longer than the configuration
/// cap (refusing, not truncating) or an empty list, and passes through an
/// entropy failure of the source of randomness.
pub(crate) fn sample<'a, R: RngCore>(
    key: &str,
    choices: &'a [BandChoice],
    rng: &mut R,
) -> Result<&'a BandChoice, VivariumError> {
    if choices.len() > MAX_CHOICES {
        return Err(VivariumError::Config(format!(
            "bands.{key} holds {} choices; at most {MAX_CHOICES} are allowed \
             (every listed entry takes a share of the work)",
            choices.len()
        )));
    }
    let Some(first) = choices.first() else {
        return Err(VivariumError::Config(format!(
            "bands.{key} holds no choices; every band needs at least one model"
        )));
    };
    if choices.len() == 1 {
        return Ok(first);
    }
    let total: u64 = choices.iter().map(|c| u64::from(c.weight())).sum();
    let zone = u64::MAX - u64::MAX % total;
    loop {
        let value = draw(rng)?;
        if value >= zone {
            continue;
        }
        // The entry whose share covers `value % total` wins. The shares sum
        // past every target below the zone, so `first` is only a start
        // value: the walk always lands inside some weight.
        let mut picked = first;
        let mut left = value % total;
        for choice in choices {
            let weight = u64::from(choice.weight());
            if left < weight {
                picked = choice;
                break;
            }
            left -= weight;
        }
        return Ok(picked);
    }
}

/// One uniform `u64` from the source of randomness: an entropy failure is
/// an error, not a panic.
fn draw<R: RngCore>(rng: &mut R) -> Result<u64, VivariumError> {
    let mut bytes = [0u8; 8];
    rng.try_fill_bytes(&mut bytes).map_err(|e| {
        VivariumError::Message(format!(
            "the band sampler lost its source of randomness: {e}"
        ))
    })?;
    Ok(u64::from_le_bytes(bytes))
}

fn not_served(pool: &str, band: u8, bands: &PoolBands) -> VivariumError {
    let serves = if bands.entries.is_empty() {
        "it serves no bands".to_string()
    } else {
        let names: Vec<&str> = bands.entries.keys().map(String::as_str).collect();
        format!("it serves {}", names.join(", "))
    };
    VivariumError::Config(format!(
        "pool {pool} has no model for band P{band} ({serves}); \
         send this job to a pool that serves P{band}, or pass --model"
    ))
}

/// The `band` key of a task's front matter.
///
/// # Errors
/// Returns an error naming the value when `band` does not parse as a band.
pub fn body_band(body: &str) -> Result<Option<u8>, VivariumError> {
    match super::job::task_front(body).band {
        Some(value) => parse_band(&value)
            .map(Some)
            .ok_or_else(|| bad_band_value(&value)),
        None => Ok(None),
    }
}

fn bad_band_value(value: &str) -> VivariumError {
    VivariumError::Config(format!(
        "the task body has \"band: {value}\": use a band P0 to P5, optionally with a -S0 to -S3 speed"
    ))
}

#[cfg(test)]
#[path = "bands_test.rs"]
mod tests;
