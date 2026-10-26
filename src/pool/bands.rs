//! Intelligence bands: parse a band string, read a task body's `band:` line,
//! and draw, weighted, the model-and-reasoning pair a pool offers for a band.
//!
//! A band is a model at an effort, never a model alone. A role's `model`
//! field holds a band string such as `P2-S0`; a task body may carry a
//! `band:` line that overrides the role's band for one assignment. A pool's
//! `[pools.<name>.bands]` table (see [`super::config_bands`]) lists the
//! weighted choices that serve a band, and every listed entry takes a share
//! of the work; the `S` speed axis is parsed and ignored.

/// Shared availability contracts: what a pool answers when asked whether it
/// can serve a band now, why it may decline, and the occupancy snapshot the
/// answer folds.
pub mod eligibility;

/// Provider failure classification: which failures cool a provider down,
/// for how long, and the cooldown receipt that records one.
pub mod provider_failure;

/// Slot reservations: one row per in-flight claim a job holds on a pool's
/// model.
pub mod reservations;

/// Admission control: the in-flight gate a pool applies before handing out
/// a slot to a job. Shell only; the gate arrives with a later slice.
pub mod admission;

use self::eligibility::{Reason, Selection, Unavailable};
use super::config::PoolConfig;
use super::config_bands::{BandChoice, PoolBands};
use crate::error::VivariumError;
use rand_core::{OsRng, RngCore};

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

/// The model-and-reasoning pair a pool offers for a band: one weighted entry
/// of the band's list, pair kept whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The band this choice serves, `0` to `5`.
    pub band: u8,
    /// Model slug the harness is run with.
    pub model: String,
    /// Reasoning effort the harness is run at.
    pub reasoning: String,
}

/// Where the one sampler draws its 64-bit words. Production draws the
/// operating system's entropy fallibly through [`SystemEntropy`]; tests
/// inject a deterministic [`rand_core`] source through [`Injected`].
pub trait RandomWords {
    /// The next 64-bit word.
    ///
    /// # Errors
    /// When the source cannot serve entropy: production answers
    /// [`VivariumError`] instead of panicking.
    fn word(&mut self) -> Result<u64, VivariumError>;
}

/// The operating system's entropy, acquired fallibly through
/// [`rand_core::OsRng`]'s [`RngCore::try_fill_bytes`]: a failed acquisition
/// answers [`VivariumError`] instead of panicking the way the trait's
/// infallible draws would.
#[derive(Debug, Clone, Copy)]
pub struct SystemEntropy;

impl RandomWords for SystemEntropy {
    fn word(&mut self) -> Result<u64, VivariumError> {
        let mut bytes = [0u8; 8];
        OsRng.try_fill_bytes(&mut bytes).map_err(|error| {
            VivariumError::Other(format!("the operating system served no entropy: {error}"))
        })?;
        Ok(u64::from_le_bytes(bytes))
    }
}

/// A [`rand_core`] random source, injected: how tests drive the one sampler
/// with deterministic words, and how a caller with its own source plugs in.
#[derive(Debug)]
pub struct Injected<R: RngCore>(pub R);

impl<R: RngCore> RandomWords for Injected<R> {
    fn word(&mut self) -> Result<u64, VivariumError> {
        Ok(self.0.next_u64())
    }
}

/// The model-and-reasoning pair `pool` offers for `band`: one entry of the
/// band's list in the pool's `[pools.<name>.bands]` table, drawn weighted
/// across the whole list with the operating system's entropy. Weightier
/// entries take a proportionally larger share, and every entry keeps its own
/// model-and-reasoning pair. Eligibility-aware callers use
/// [`choose_eligible`] instead.
///
/// # Errors
/// Returns an error naming the band, the pool and the bands the pool does
/// serve when the pool has nothing for the band: send the job to a pool that
/// serves it, or pass `--model`. Also answers [`VivariumError`] when the
/// operating system serves no entropy.
pub fn choose(config: &PoolConfig, pool: &str, band: u8) -> Result<Choice, VivariumError> {
    let eligible: Vec<&BandChoice> = band_entries(config, pool, band)?.iter().collect();
    let mut words = SystemEntropy;
    Ok(chosen(band, weighted_pick(&eligible, &mut words)?))
}

/// The model-and-reasoning pair `pool` offers for `band`, drawn weighted
/// across the entries no exclusion rules out: [`choose`] with eligibility.
/// `excluded` is asked once per listed entry with that entry's model, and
/// `Some` rules the entry out for now. When the band is configured and
/// nonempty yet every entry is ruled out, the pool answers
/// [`Selection::Unavailable`] with the exclusions folded into one stable
/// reason and the earliest retry deadline any exclusion names.
///
/// # Errors
/// The missing-or-empty-band error [`choose`] answers, and whatever entropy
/// acquisition fails with through `words`.
pub fn choose_eligible<W, E>(
    config: &PoolConfig,
    pool: &str,
    band: u8,
    words: &mut W,
    mut excluded: E,
) -> Result<Selection<Choice>, VivariumError>
where
    W: RandomWords,
    E: FnMut(&str) -> Option<Unavailable>,
{
    let mut eligible: Vec<&BandChoice> = Vec::new();
    let mut exclusions: Vec<Unavailable> = Vec::new();
    for entry in band_entries(config, pool, band)? {
        match excluded(&entry.model) {
            Some(exclusion) => exclusions.push(exclusion),
            None => eligible.push(entry),
        }
    }
    if eligible.is_empty() {
        return Ok(Selection::Unavailable(aggregate(&exclusions)));
    }
    Ok(Selection::Selected(chosen(
        band,
        weighted_pick(&eligible, words)?,
    )))
}

/// The configured, nonempty list of choices for `band`, or the configuration
/// error when the pool misses the band or leaves its list empty.
fn band_entries<'a>(
    config: &'a PoolConfig,
    pool: &str,
    band: u8,
) -> Result<&'a [BandChoice], VivariumError> {
    match config.bands.entries.get(&format!("P{band}")) {
        Some(list) if !list.is_empty() => Ok(list),
        _ => Err(not_served(pool, band, &config.bands)),
    }
}

/// The one sampler both entry points share: the entries weighted by their
/// `weight` fields, summed in `u64` so `u32::MAX` choices cannot overflow,
/// and one drawn by unbiased bounded sampling. Every weight is at least 1
/// (checked when the bands table loads), so `total` is at least 1; the
/// caller guarantees at least one entry.
fn weighted_pick<'a, W: RandomWords>(
    eligible: &[&'a BandChoice],
    words: &mut W,
) -> Result<&'a BandChoice, VivariumError> {
    let total: u64 = eligible.iter().map(|entry| u64::from(entry.weight)).sum();
    let mut ticket = bounded(words, total)?;
    for entry in &eligible[..eligible.len() - 1] {
        let weight = u64::from(entry.weight);
        if ticket < weight {
            return Ok(entry);
        }
        ticket -= weight;
    }
    Ok(eligible[eligible.len() - 1])
}

/// A uniform sample in `0..bound` from `words`: draws that would let the
/// remainder tilt the range are rejected and redrawn, so no index of the
/// range is any likelier than another.
fn bounded<W: RandomWords>(words: &mut W, bound: u64) -> Result<u64, VivariumError> {
    let keep_below = u64::MAX - u64::MAX % bound;
    loop {
        let draw = words.word()?;
        if draw < keep_below {
            return Ok(draw % bound);
        }
    }
}

/// Fold every entry's exclusion into one answer: the exclusion that lifts
/// soonest carries the reason and the retry deadline — an earlier deadline
/// wins, a known deadline beats an unknown one, list order breaks ties — so
/// the same exclusions always fold the same way.
fn aggregate(exclusions: &[Unavailable]) -> Unavailable {
    let mut binding: Option<&Unavailable> = None;
    for exclusion in exclusions {
        binding = Some(match binding {
            None => exclusion,
            Some(kept) => match (kept.retry_at, exclusion.retry_at) {
                (Some(kept_at), Some(lifts)) if lifts < kept_at => exclusion,
                (None, Some(_)) => exclusion,
                _ => kept,
            },
        });
    }
    binding.cloned().unwrap_or(Unavailable {
        reason: Reason::BandUnavailable,
        retry_at: None,
    })
}

/// The choice a band entry serves: pair kept whole.
fn chosen(band: u8, entry: &BandChoice) -> Choice {
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
