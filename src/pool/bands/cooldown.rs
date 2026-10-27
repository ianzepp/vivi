//! Cooldown evidence: what a provider failure is worth, in absolute time.
//!
//! Evidence records one observed failure as the provider it cools down,
//! the absolute epoch second the cooldown ends at, and the stable failure
//! class the failure was classified as. Grouping model slugs into
//! providers is [`provider_key`], so every slug a provider serves cools
//! down together. Classifying a live failure into evidence is live I/O
//! and lives in [`super::live`], not here. Replaying recorded evidence
//! across every pool of one mailspace is [`fold`]: pure reading of the
//! recorded notes, no live I/O.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::mailspace::PoolEventRow;
use crate::pool::collect::Outcome;

/// How long one classified failure cools its provider down.
const COOLDOWN_SECS: u64 = 60;

/// Classify one finished dispatch as cooldown evidence for `model`'s
/// [`provider_key`]: `Some(evidence)` only for a finished harness execution
/// with a nonzero agent exit whose stderr or harness output carries
/// provider-error-shaped evidence of HTTP 429 / rate limiting or explicit
/// quota exhaustion (structured `code` values such as `insufficient_quota`,
/// `quota-exceeded` and usage-limit codes). Timeouts, lost slots, missing or
/// unparsable exit statuses and ordinary (non-provider) failures never
/// classify. The evidence takes the provider from the model slug prefix,
/// never a machine host, and ends a fixed [`COOLDOWN_SECS`] after
/// `collected_at`, the moment the failure was collected — never dispatch
/// time.
#[must_use]
pub fn classify(model: &str, outcome: &Outcome, collected_at: DateTime<Utc>) -> Option<Evidence> {
    let Outcome::Finished(collected) = outcome else {
        return None;
    };
    // A missing or unparsable exit status is not a provider failure.
    if !collected
        .result
        .get("agent_rc")
        .parse::<i64>()
        .is_ok_and(|rc| rc != 0)
    {
        return None;
    }
    let text = format!("{}\n{}", collected.stderr, collected.receipt);
    let class = classify_text(&text)?;
    let until = u64::try_from(collected_at.timestamp())
        .unwrap_or(0)
        .saturating_add(COOLDOWN_SECS);
    Some(Evidence {
        provider: provider_key(model).to_string(),
        until,
        class,
    })
}

/// The failure class provider-error-shaped text carries: explicit quota
/// codes win over rate limiting when both appear. Matching is deliberately
/// narrow so prose about provider errors — a receipt discussing a 429,
/// example code naming a quota code — and other transient failures
/// (connection errors, overload alone) never classify.
fn classify_text(text: &str) -> Option<FailureClass> {
    if structured_quota_codes(text) {
        return Some(FailureClass::Quota);
    }
    if rate_limited(text) {
        return Some(FailureClass::RateLimit);
    }
    None
}

/// How a failure that produced cooldown evidence was classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureClass {
    /// The provider refused the request for sending too much, too fast.
    RateLimit,
    /// The provider refused the request for exhausted credit or quota.
    Quota,
}

impl FailureClass {
    /// The stable word this class answers as in reports and letters.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::RateLimit => "rate-limit",
            Self::Quota => "quota",
        }
    }
}

/// One observed provider failure, held as absolute time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// The provider the cooldown applies to, a [`provider_key`].
    pub provider: String,
    /// The absolute epoch second the cooldown ends at.
    pub until: u64,
    /// The failure class the failure was recorded as.
    pub class: FailureClass,
}

/// The note key a collected record carries its cooldown evidence under:
/// a namespaced object shaped exactly like [`Evidence`]. Notes are
/// extensible JSON, so every other key — including the pool that wrote
/// the record — is another module's namespace, never a scope here.
const BAND_COOLDOWN_KEY: &str = "band_cooldown";

/// Replay recorded cooldown evidence across every pool of one mailspace:
/// the maximum unexpired absolute deadline per provider, keyed by
/// [`provider_key`]. Evidence travels in the [`BAND_COOLDOWN_KEY`] object
/// of `pool_collected` notes; a note without metadata and malformed or
/// non-object metadata are evidence of nothing, as is any event of
/// another type. Deadlines are absolute: folding never restarts one from
/// `now`, never clears an earlier failure because a success follows, and
/// at `now == until` the evidence is expired. Successes carry no
/// metadata, so the fold is order-independent and replaying it again is
/// a fixed point. The live snapshot owns calling this fold; nothing here
/// performs live I/O.
#[must_use]
pub fn fold(events: &[PoolEventRow], now: u64) -> BTreeMap<String, u64> {
    let mut deadlines: BTreeMap<String, u64> = BTreeMap::new();
    for evidence in events.iter().filter_map(recorded_evidence) {
        if evidence.until <= now {
            continue;
        }
        let until = deadlines.entry(evidence.provider).or_default();
        if evidence.until > *until {
            *until = evidence.until;
        }
    }
    deadlines
}

/// The cooldown evidence one event's note records, when it carries any:
/// only a `pool_collected` note whose JSON holds a well-formed
/// [`BAND_COOLDOWN_KEY`] object shaped like [`Evidence`] counts.
/// Arbitrary receipt strings never become evidence.
fn recorded_evidence(event: &PoolEventRow) -> Option<Evidence> {
    if event.event_type != "pool_collected" {
        return None;
    }
    let note = match serde_json::from_str::<serde_json::Value>(&event.note) {
        Ok(note) => note,
        Err(_) => return None,
    };
    let metadata = note.get(BAND_COOLDOWN_KEY)?;
    match Evidence::deserialize(metadata) {
        Ok(evidence) => Some(evidence),
        Err(_) => None,
    }
}

/// The provider a model slug belongs to: the prefix before the first
/// slash, or the entire slug when it is unqualified. `zai/glm-5.3-flash`
/// cools down `zai`; `glm-5.3-flash` is a provider of its own.
#[must_use]
pub fn provider_key(model: &str) -> &str {
    match model.split_once('/') {
        Some((provider, _)) => provider,
        None => model,
    }
}

/// Provider quota-exhaustion codes that travel as JSON `code` values.
/// Matched lowercased against the quoted shape (colon, optional space),
/// which prose and example code never carry. Prefix matching so suffixed
/// codes such as `usage_limit_reached` count.
const QUOTA_CODES: &[&str] = &[
    "insufficient_quota",
    "quota-exceeded",
    "quota_exceeded",
    "usage_limit",
];

/// Whether the text names quota exhaustion as a structured provider `code`
/// value (`"code":"insufficient_quota"`, optionally spaced after the
/// colon), not as prose.
fn structured_quota_codes(text: &str) -> bool {
    let lower = text.to_lowercase();
    QUOTA_CODES.iter().any(|code| {
        lower.contains(&format!("\"code\":\"{code}"))
            || lower.contains(&format!("\"code\": \"{code}"))
    })
}

/// Wording a provider uses to name rate limiting, lowercased.
const RATE_LIMIT_WORDS: &[&str] = &["rate limit", "rate-limit", "ratelimit", "too many requests"];

/// Shapes a refused HTTP 429 status takes in provider output, lowercased:
/// a status, code or body marker, never a bare year-or-number substring.
const STATUS_429: &[&str] = &["status 429", "http 429", "code 429", "error 429", "429:"];

/// Whether the text is provider-error-shaped evidence of rate limiting: a
/// refused 429 status whose own or following line names the rate limiting
/// (the checked-in Z.AI-style pair `post to provider failed: status 429`
/// then `upstream error code 1302: rate limited`), or a rate-limit verdict
/// carrying a provider error code of its own — stderr and receipts are
/// kept as tails, so a cut-off 429 line can leave only that verdict.
fn rate_limited(text: &str) -> bool {
    let lines: Vec<String> = text.lines().map(str::to_lowercase).collect();
    for (index, line) in lines.iter().enumerate() {
        if !STATUS_429.iter().any(|marker| line.contains(marker)) {
            continue;
        }
        if RATE_LIMIT_WORDS.iter().any(|word| line.contains(word)) {
            return true;
        }
        if let Some(next) = lines.get(index + 1) {
            if RATE_LIMIT_WORDS.iter().any(|word| next.contains(word)) {
                return true;
            }
        }
    }
    lines.iter().any(|line| {
        RATE_LIMIT_WORDS.iter().any(|word| line.contains(word))
            && (line.contains("upstream") || line.contains("error code"))
    })
}

#[cfg(test)]
#[path = "cooldown_test.rs"]
mod tests;
