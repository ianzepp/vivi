//! Provider-health vocabulary: which provider a model belongs to, what one
//! rate-limit or quota observation records, and the conservative classifier
//! that turns one collected dispatch into such an observation.
//!
//! A provider key is derived from the model slug alone, never from the
//! pool's configured provider: one pool may serve models from several
//! upstreams, so the slug is the truth. The classifier reads only the
//! collected receipt, stderr, and the agent's execution result — never the
//! runner log or task prose — and stays narrow on purpose: a wrong
//! observation idles a healthy provider. Nothing here performs I/O:
//! [`cooldowns`] replays stored observations from an event log in memory,
//! and a later slice wires it into admission.

use std::collections::BTreeMap;

use chrono::Duration;
use serde::{Deserialize, Serialize};

use crate::mailspace::PoolEventRow;
use crate::pool::collect::{Collected, JobResult, Outcome};
use crate::pool::record::DispatchRecord;

/// The default cooldown, in seconds, applied to a provider after one
/// rate-limit or quota observation.
pub const COOLDOWN_SECS: u64 = 60;

/// The namespaced `pool_collected` note key under whose value a
/// [`HealthObservation`] is stored in the note's JSON. Dotted with a
/// `pool.` prefix so it cannot collide with a job's plain result fields.
pub const COLLECTED_NOTE_KEY: &str = "pool.health";

/// Which upstream failure an observation saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// The upstream answered "slow down": a transient rate limit.
    RateLimit,
    /// The upstream answered "no budget": the key's quota is spent.
    Quota,
}

/// One provider-health observation: which provider, what was seen, when,
/// and until when the provider is cooling down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthObservation {
    /// The provider key the failing model slug maps to (see
    /// [`provider_key`]).
    pub provider: String,
    /// What the upstream failure was.
    pub classification: Classification,
    /// When the failure was observed.
    pub observed_at: chrono::DateTime<chrono::Utc>,
    /// When the provider may be tried again: `observed_at` plus
    /// [`COOLDOWN_SECS`].
    pub cooldown_until: chrono::DateTime<chrono::Utc>,
}

impl HealthObservation {
    /// Record one observation of `provider` seen at `now`; the provider
    /// cools down for [`COOLDOWN_SECS`] seconds from then.
    #[must_use]
    pub fn at(
        provider: &str,
        classification: Classification,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            provider: provider.to_string(),
            classification,
            observed_at: now,
            cooldown_until: now + Duration::seconds(COOLDOWN_SECS as i64),
        }
    }
}

/// The provider key of a model slug: the component before the first slash,
/// or the entire slug when it carries no slash. `zai/glm-5.3-flash` keys to
/// `zai`; `glm-5.3-flash` keys to itself. Never the pool's configured
/// provider.
#[must_use]
pub fn provider_key(model: &str) -> &str {
    match model.split_once('/') {
        Some((provider, _rest)) => provider,
        None => model,
    }
}

/// Classify one collected dispatch conservatively: `Some` observation when
/// its receipt or stderr carries provider rate-limit or quota evidence,
/// `None` otherwise. The provider key comes from the trusted
/// [`DispatchRecord`] model, `now` is the collection instant, and the
/// cooldown is the fixed [`COOLDOWN_SECS`]. A timeout or lost slot carries
/// no evidence, and a no-model command job has no provider to cool.
#[must_use]
pub fn observation(
    record: &DispatchRecord,
    outcome: &Outcome,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<HealthObservation> {
    let collected = match outcome {
        Outcome::Finished(collected) => collected,
        Outcome::Timeout | Outcome::Lost => return None,
    };
    if record.model.is_empty() {
        return None;
    }
    Some(HealthObservation::at(
        provider_key(&record.model),
        classify(collected)?,
        now,
    ))
}

/// The provider cooldown deadlines replayed from stored observations:
/// every `pool_collected` event collected on `pool` whose note carries a
/// [`COLLECTED_NOTE_KEY`] [`HealthObservation`] contributes its stored
/// `cooldown_until`, and each provider keeps the maximum. Pure replay over
/// the log: nothing is recomputed and nothing adds [`COOLDOWN_SECS`] to any
/// clock, an event of another pool is skipped, and a note without the
/// metadata (older binaries) or with malformed metadata contributes
/// nothing — a success never clears a deadline, because only observations
/// fold in. Which deadlines are still active is the caller's call at its
/// injected instant, exactly as `Availability` decides it at `now >=
/// deadline`.
#[must_use]
pub fn cooldowns(
    events: &[PoolEventRow],
    pool: &str,
) -> BTreeMap<String, chrono::DateTime<chrono::Utc>> {
    let mut deadlines: BTreeMap<String, chrono::DateTime<chrono::Utc>> = BTreeMap::new();
    for event in events {
        if event.event_type != "pool_collected" {
            continue;
        }
        let Ok(note) = serde_json::from_str::<serde_json::Value>(&event.note) else {
            continue;
        };
        if note.get("pool").and_then(|value| value.as_str()) != Some(pool) {
            continue;
        }
        let Some(stored) = note.get(COLLECTED_NOTE_KEY) else {
            continue;
        };
        let Ok(seen) = serde_json::from_value::<HealthObservation>(stored.clone()) else {
            continue;
        };
        let deadline = deadlines
            .entry(seen.provider)
            .or_insert(seen.cooldown_until);
        if seen.cooldown_until > *deadline {
            *deadline = seen.cooldown_until;
        }
    }
    deadlines
}

/// The rate-limit or quota classification of one collected run, or `None`
/// when its receipt and stderr carry no qualifying evidence.
fn classify(collected: &Collected) -> Option<Classification> {
    let evidence = format!("{}\n{}", collected.receipt, collected.stderr);
    // A structured provider error payload qualifies on its shape alone,
    // even when the agent exited 0 (an error-only response).
    if let Some(classification) = structured(&evidence) {
        return Some(classification);
    }
    // Unstructured wording only counts when the agent's execution itself
    // failed: a publication problem is not a provider failure.
    if !agent_failed(&collected.result) {
        return None;
    }
    names_http_429(&evidence.to_lowercase()).then_some(Classification::RateLimit)
}

/// Structured provider error payloads: the known zai `429: {"code":"1302"`
/// response and provider error codes carried as a JSON `code` value.
fn structured(text: &str) -> Option<Classification> {
    if is_known_429_1302(text) || has_json_code(text, "rate_limit_exceeded") {
        return Some(Classification::RateLimit);
    }
    if has_json_code(text, "insufficient_quota") || has_json_code(text, "quota_exceeded") {
        return Some(Classification::Quota);
    }
    None
}

/// `429: {"code":"1302",...}` — the exact throttle response a zai call
/// leaves behind (whitespace after the colon tolerated).
fn is_known_429_1302(text: &str) -> bool {
    text.contains("429:{\"code\":\"1302\"") || text.contains("429: {\"code\":\"1302\"")
}

/// Whether `code` appears as a JSON `code` field's value, the shape a
/// provider error payload carries (`"code":"insufficient_quota"`). A bare
/// word in prose or example code does not match.
fn has_json_code(text: &str, code: &str) -> bool {
    text.contains(&format!("\"code\":\"{code}\""))
        || text.contains(&format!("\"code\": \"{code}\""))
}

/// Free-text HTTP 429 wording: `HTTP 429`, `HTTP/1.1 429`, `status 429`,
/// `429 Too Many Requests`. Matched against lowercased text; a bare `429`
/// number never matches.
fn names_http_429(lower: &str) -> bool {
    lower.contains("http 429")
        || lower.contains("status 429")
        || lower.contains("status: 429")
        || lower.contains("status code 429")
        || lower.contains("429 too many requests")
        || ["/1.0 ", "/1.1 ", "/2 ", "/3 "]
            .iter()
            .any(|version| lower.contains(&format!("http{version}429")))
}

/// Whether the agent's execution itself failed: `agent_rc` present and
/// nonzero. Absent or unreadable counts as not failed.
fn agent_failed(result: &JobResult) -> bool {
    result
        .get("agent_rc")
        .parse::<i64>()
        .is_ok_and(|rc| rc != 0)
}

#[cfg(test)]
#[path = "health_test.rs"]
mod tests;
