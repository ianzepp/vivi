//! Intelligence bands: parse a band string, read a task body's `band:` line,
//! and choose the model-and-reasoning pair a pool offers for a band.
//!
//! A band is a model at an effort, never a model alone. A role's `model`
//! field holds a band string such as `P2-S0`; a task body may carry a
//! `band:` line that overrides the role's band for one assignment. A pool's
//! `[pools.<name>.bands]` table (see [`super::config_bands`]) says which
//! choice serves which band; the `S` speed axis is parsed and ignored.

use super::config::PoolConfig;
use super::config_bands::PoolBands;
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

/// The model-and-reasoning pair a pool offers for a band: the first entry of
/// the band's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The band this choice serves, `0` to `5`.
    pub band: u8,
    /// Model slug the harness is run with.
    pub model: String,
    /// Reasoning effort the harness is run at.
    pub reasoning: String,
}

/// The model-and-reasoning pair `pool` offers for `band`: the first entry of
/// the band's list in the pool's `[pools.<name>.bands]` table. Later entries
/// of the list are reserved and never chosen here.
///
/// # Errors
/// Returns an error naming the band, the pool and the bands the pool does
/// serve when the pool has nothing for the band: send the job to a pool that
/// serves it, or pass `--model`.
pub fn choose(config: &PoolConfig, pool: &str, band: u8) -> Result<Choice, VivariumError> {
    let Some(entry) = config
        .bands
        .entries
        .get(&format!("P{band}"))
        .and_then(|list| list.first())
    else {
        return Err(not_served(pool, band, &config.bands));
    };
    Ok(Choice {
        band,
        model: entry.model.clone(),
        reasoning: entry.reasoning.clone(),
    })
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

/// The first `band:` line of a task body, with the same trimming and
/// list-dash rules as `from:` (see [`super::job::body_refs`]): surrounding
/// whitespace and a leading list dash are ignored, a value holding spaces is
/// skipped, and the first value wins.
///
/// # Errors
/// Returns an error naming the value when a `band:` line does not parse as a
/// band.
pub fn body_band(body: &str) -> Result<Option<u8>, VivariumError> {
    for line in body.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.contains(char::is_whitespace) {
            continue;
        }
        if key.trim().to_ascii_lowercase() == "band" {
            return parse_band(value)
                .map(Some)
                .ok_or_else(|| bad_band_value(value));
        }
    }
    Ok(None)
}

fn bad_band_value(value: &str) -> VivariumError {
    VivariumError::Config(format!(
        "the task body has \"band: {value}\": use a band P0 to P5, optionally with a -S0 to -S3 speed"
    ))
}

#[cfg(test)]
#[path = "bands_test.rs"]
mod tests;
