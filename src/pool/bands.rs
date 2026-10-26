//! Intelligence bands: parse a band string, read a task body's `band:` line,
//! and choose the model-and-reasoning pair a pool offers for a band.
//!
//! A band is a model at an effort, never a model alone. A role's `model`
//! field holds a band string such as `P2-S0`; a task body may carry a
//! `band:` line that overrides the role's band for one assignment. A pool's
//! `[pools.<name>.bands]` table (see [`super::config_bands`]) lists the
//! choices that serve a band, and every listed entry takes a share of the
//! work by its checked `weight`; the `S` speed axis is parsed and ignored.

use std::io;

use super::config::PoolConfig;
use super::config_bands::PoolBands;
use crate::error::VivariumError;

pub mod admission;
pub mod availability;
pub mod guard;
pub mod provider_error;
pub mod runtime;
pub mod types;

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

/// The model-and-reasoning pair a pool offers for a band: one entry of the
/// band's list, drawn by checked weight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The band this choice serves, `0` to `5`.
    pub band: u8,
    /// Model slug the harness is run with.
    pub model: String,
    /// Reasoning effort the harness is run at.
    pub reasoning: String,
}

/// The model-and-reasoning pair `pool` offers for `band`: one entry of the
/// band's list in the pool's `[pools.<name>.bands]`, drawn fresh on every
/// call. It is not a primary with reserved spares and not first-then-fallback:
/// every listed entry takes a share of the work, the share of its checked
/// weight. The draw is fresh OS-backed randomness, never seeded from time,
/// process or job identifiers; a band listing exactly one entry is returned
/// directly, without drawing randomness.
///
/// # Errors
/// Returns an error naming the band, the pool and the bands the pool does
/// serve when the pool has nothing for the band: send the job to a pool that
/// serves it, or pass `--model`. Returns the checked-weight error when an
/// entry carries an invalid `weight` even though it was built without
/// validation, and an I/O error when the operating system will not hand over
/// entropy; an entropy failure never falls back to a fixed entry.
pub fn choose(config: &PoolConfig, pool: &str, band: u8) -> Result<Choice, VivariumError> {
    choose_with(config, pool, band, &mut Os)
}

/// [`choose`] over an injected draw source, the deterministic seam tests
/// drive with canned values instead of real entropy.
fn choose_with(
    config: &PoolConfig,
    pool: &str,
    band: u8,
    draw: &mut dyn Draw,
) -> Result<Choice, VivariumError> {
    let key = format!("P{band}");
    let Some(list) = config
        .bands
        .entries
        .get(&key)
        .filter(|list| !list.is_empty())
    else {
        return Err(not_served(pool, band, &config.bands));
    };
    let mut cumulative = Vec::with_capacity(list.len());
    let mut total: u64 = 0;
    for entry in list {
        total += u64::from(entry.effective_weight(pool, &key)?);
        cumulative.push(total);
    }
    let index = if list.len() == 1 {
        0
    } else {
        let target = draw.select(total)?;
        cumulative.partition_point(|&reached| reached <= target)
    };
    let entry = &list[index];
    Ok(Choice {
        band,
        model: entry.model.clone(),
        reasoning: entry.reasoning.clone(),
    })
}

/// The draw boundary of a band selection: where the drawn value comes from.
/// Production draws fresh OS entropy; tests inject canned values to stay
/// repeatable.
trait Draw {
    /// A uniform value in `0..below`, or an error when entropy fails. No
    /// value of the range may be more likely than another.
    fn select(&mut self, below: u64) -> Result<u64, VivariumError>;
}

/// The production draw: fresh OS-backed randomness.
struct Os;

impl Draw for Os {
    fn select(&mut self, below: u64) -> Result<u64, VivariumError> {
        if below <= 1 {
            return Ok(0);
        }
        loop {
            let mut word = [0u8; 8];
            getrandom::fill(&mut word).map_err(|error| {
                VivariumError::Io(io::Error::other(format!(
                    "the band draw cannot read OS entropy: {error}"
                )))
            })?;
            if let Some(value) = bounded(u64::from_le_bytes(word), below) {
                return Ok(value);
            }
        }
    }
}

/// The unbiased bounded reduction of one fresh word to `0..below`: mask the
/// word to the power of two covering the range and reject anything still at
/// or above `below`, so every value of the range is equally likely.
fn bounded(word: u64, below: u64) -> Option<u64> {
    let value = word & (below.next_power_of_two() - 1);
    (value < below).then_some(value)
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
