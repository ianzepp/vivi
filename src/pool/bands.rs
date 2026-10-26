//! Intelligence bands: parse a band string, read a task body's `band:` line,
//! and choose — weighted at random across every entry — the
//! model-and-reasoning pair a pool offers for a band.
//!
//! A band is a model at an effort, never a model alone. A role's `model`
//! field holds a band string such as `P2-S0`; a task body may carry a
//! `band:` line that overrides the role's band for one assignment. A pool's
//! `[pools.<name>.bands]` table (see [`super::config_bands`]) says which
//! choices serve which band; the `S` speed axis is parsed and ignored.
//! Every listed entry takes a share of the work sized by its `weight` —
//! there is no primary with reserved spares — and [`availability`] is the
//! view of which entries can take work right now.

pub mod availability;
pub mod health;

use rand_core::{OsRng, RngCore};

use self::availability::Availability;
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
/// band's list, drawn at random.
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
/// band's list in the pool's `[pools.<name>.bands]` table, drawn at random
/// so that every listed entry takes a share of the work sized by its
/// `weight`. This is not a primary with reserved spares: no entry is held
/// back, and duplicate entries hold their own shares.
///
/// # Errors
/// Returns an error naming the band, the pool and the bands the pool does
/// serve when the pool has nothing for the band: send the job to a pool that
/// serves it, or pass `--model`.
pub fn choose(config: &PoolConfig, pool: &str, band: u8) -> Result<Choice, VivariumError> {
    select(config, pool, band, &Availability::default(), &mut OsDraw)?
        .ok_or_else(|| not_served(pool, band, &config.bands))
}

/// The model-and-reasoning pair `pool` offers for `band` among the entries
/// that can take work at `availability`: the same weighted draw as
/// [`choose`], with the entries the view blocks removed before the weights
/// are normalized and summed. [`None`] when a configured band holds no
/// available entry.
///
/// # Errors
/// Returns the same error [`choose`] returns when the pool has nothing for
/// the band at all — no bands table, or a missing or empty list.
pub fn choose_eligible(
    config: &PoolConfig,
    pool: &str,
    band: u8,
    availability: &Availability,
) -> Result<Option<Choice>, VivariumError> {
    select(config, pool, band, availability, &mut OsDraw)
}

/// One uniform draw in `[0, 1)` from a random source, or the error the
/// source raised. [`OsDraw`] is the production source; tests inject a
/// deterministic one.
trait Draw {
    fn draw(&mut self) -> Result<f64, VivariumError>;
}

/// The production draw source: fresh entropy from the operating system.
struct OsDraw;

impl Draw for OsDraw {
    fn draw(&mut self) -> Result<f64, VivariumError> {
        let mut bytes = [0u8; 8];
        OsRng.try_fill_bytes(&mut bytes).map_err(|_| {
            VivariumError::Other("entropy unavailable: cannot draw a band choice".into())
        })?;
        Ok((u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64)
    }
}

/// The shared core of [`choose`] and [`choose_eligible`]: resolve the
/// band's list, drop the entries `availability` blocks, and draw one of
/// what is left. A missing or empty list is a configuration error; a list
/// whose every entry is blocked has no choice.
fn select(
    config: &PoolConfig,
    pool: &str,
    band: u8,
    availability: &Availability,
    source: &mut dyn Draw,
) -> Result<Option<Choice>, VivariumError> {
    let Some(list) = config
        .bands
        .entries
        .get(&format!("P{band}"))
        .filter(|list| !list.is_empty())
    else {
        return Err(not_served(pool, band, &config.bands));
    };
    let eligible: Vec<&BandChoice> = list
        .iter()
        .filter(|entry| availability.model_available(&entry.model))
        .collect();
    if eligible.is_empty() {
        return Ok(None);
    }
    weighted(band, &eligible, source).map(Some)
}

/// Draw one of `eligible` with each entry's share sized by its weight,
/// normalized before summing. A single entry answers without consulting the
/// draw source, so one choice consumes no entropy; an entropy failure is an
/// error, never a fallback to the first entry.
fn weighted(
    band: u8,
    eligible: &[&BandChoice],
    source: &mut dyn Draw,
) -> Result<Choice, VivariumError> {
    if let [only] = eligible {
        return Ok(choice(band, only));
    }
    let weights: Vec<f64> = eligible
        .iter()
        .map(|entry| entry.weight())
        .collect::<Result<_, _>>()?;
    let total: f64 = weights.iter().sum();
    let mut taken = source.draw()?;
    for (entry, weight) in eligible.iter().zip(&weights) {
        taken -= weight / total;
        if taken < 0.0 {
            return Ok(choice(band, entry));
        }
    }
    // The draw lands above every cumulative weight only through float
    // rounding; the last entry owns the top of the range.
    Ok(choice(band, eligible[eligible.len() - 1]))
}

fn choice(band: u8, entry: &BandChoice) -> Choice {
    Choice {
        band,
        model: entry.model.clone(),
        reasoning: entry.reasoning.clone(),
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
