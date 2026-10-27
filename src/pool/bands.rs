//! Intelligence bands: parse a band string, read a task body's `band:` line,
//! and choose the model-and-reasoning pair a pool offers for a band.
//!
//! A band is a model at an effort, never a model alone. A role's `model`
//! field holds a band string such as `P2-S0`; a task body may carry a
//! `band:` line that overrides the role's band for one assignment. A pool's
//! `[pools.<name>.bands]` table (see [`super::config_bands`]) lists the
//! model-and-reasoning choices that serve a band; every listed entry takes
//! a share of the work, sized by its `weight`, so a band's choice is a
//! weighted sample over the whole list. The `S` speed axis is parsed and
//! ignored.

pub mod cooldown;
pub mod guard;
pub mod live;
pub mod runtime;

use super::config::PoolConfig;
use super::config_bands::{BandChoice, PoolBands};
use crate::error::VivariumError;

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
/// band's list, sampled by relative weight.
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
/// band's list in the pool's `[pools.<name>.bands]` table, sampled by
/// relative weight. No entry is a primary with reserved spares: every
/// listed entry takes a share of the work. A single-entry band answers
/// without consulting the entropy source.
///
/// # Errors
/// Returns an error naming the band, the pool and the bands the pool does
/// serve when the pool has nothing for the band: send the job to a pool that
/// serves it, or pass `--model`. Returns an error when the entropy source
/// fails or the list breaks the shape configuration enforces.
pub fn choose(config: &PoolConfig, pool: &str, band: u8) -> Result<Choice, VivariumError> {
    let served = config
        .bands
        .entries
        .get(&format!("P{band}"))
        .filter(|list| !list.is_empty());
    let Some(list) = served else {
        return Err(not_served(pool, band, &config.bands));
    };
    let entry = select(list, &mut OsDraw)?;
    Ok(Choice {
        band,
        model: entry.model.clone(),
        reasoning: entry.reasoning.clone(),
    })
}

/// The most entries of one band's list the sampler serves. Configuration
/// (`PoolConfig::check_bands`) refuses longer lists before a pool loads;
/// the sampler refuses them too instead of silently truncating to the
/// first entries.
const MAX_ENTRIES: usize = 4;

/// Sample one entry of a band's list by relative weight: the shared seam
/// `bands::runtime` resolution can call directly, with any [`Draw`] source.
/// One eligible entry is returned without consulting `draw`; a list longer
/// than configuration allows is refused, not truncated.
///
/// # Errors
/// Returns a configuration error for an empty or oversized list and the
/// draw's error when the entropy source fails.
pub(super) fn select<'a>(
    choices: &'a [BandChoice],
    draw: &mut dyn Draw,
) -> Result<&'a BandChoice, VivariumError> {
    if choices.is_empty() {
        return Err(VivariumError::Config(
            "the band list is empty; it serves no model".to_string(),
        ));
    }
    if choices.len() > MAX_ENTRIES {
        return Err(VivariumError::Config(format!(
            "the band list holds {} choices; at most {MAX_ENTRIES} are allowed",
            choices.len()
        )));
    }
    if let [only] = choices {
        return Ok(only);
    }
    weighted(choices, draw)
}

/// The weighted draw over a list of two or more entries: a uniform u64
/// reduced over the weight total, rejection-sampled so the reduction stays
/// unbiased, then walked over the entries' cumulative shares.
fn weighted<'a>(
    choices: &'a [BandChoice],
    draw: &mut dyn Draw,
) -> Result<&'a BandChoice, VivariumError> {
    let total: u64 = choices.iter().map(|c| u64::from(c.weight())).sum();
    // Values at or above the largest multiple of `total` in the u64 range
    // would bias the low residues, so they are drawn again.
    let zone = u64::MAX - u64::MAX % total;
    loop {
        let drawn = draw_u64(draw)?;
        if drawn >= zone {
            continue;
        }
        let mut left = drawn % total;
        for choice in choices {
            let share = u64::from(choice.weight());
            if left < share {
                return Ok(choice);
            }
            left -= share;
        }
        // `left` began below `total` and every share was subtracted, so the
        // walk above always returns; this line is only its exhaustive form.
        return Ok(&choices[choices.len() - 1]);
    }
}

/// One uniform u64 from the draw source, big-endian over eight bytes.
fn draw_u64(draw: &mut dyn Draw) -> Result<u64, VivariumError> {
    let mut bytes = [0u8; 8];
    draw.fill(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

/// A fallible uniform random source: the seam deterministic tests inject
/// canned or failing sources through, and [`super::runtime`] resolution
/// shares the sampler behind.
trait Draw {
    /// Fill `dest` with uniform random bytes.
    fn fill(&mut self, dest: &mut [u8]) -> Result<(), VivariumError>;
}

/// The production draw: the operating system's entropy source.
struct OsDraw;

impl Draw for OsDraw {
    fn fill(&mut self, dest: &mut [u8]) -> Result<(), VivariumError> {
        getrandom::fill(dest)
            .map_err(|e| VivariumError::Message(format!("the entropy source failed: {e}")))
    }
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
