//! Provider identity, per-provider backoff buckets, and configured
//! ceilings for pump admission.
//!
//! A provider is a *model* provider, derived only from the executed model
//! slug: `zai/glm-5.3-flash` and `zai/glm-5.3` share the `zai` namespace,
//! while a bare slug is opaque. [`PoolConfig::provider`] names the machine
//! host — the compute adapter — and is never a provider identity here, and
//! no code in this module reads credentials.
//!
//! Scope is the current mailspace and pool ([`ProviderScope`], from
//! [`PoolId`]); a repository target never takes part. Bucket state is
//! serializable so state persistence can store it, and
//! [`AdmissionSnapshot`] freezes the per-provider facts an admission
//! decision was made from.
//!
//! The policy is fully deterministic: a distinct rate-limit observation
//! halves the learned allowance (floored at one while capacity is
//! enabled), every cooldown earns the exponential retry base plus the
//! 0-14 second [`jitter`] spread, and after a 300-second quiet interval
//! exactly one slot recovers. Running dispatches are never cancelled; the
//! policy gates fresh starts only.

use std::collections::{BTreeMap, BTreeSet};

use super::transient::jitter;
use serde::{Deserialize, Serialize};

use crate::pool::config::PoolConfig;
use crate::pool::slot::PoolId;

/// The concurrent-dispatch allowance a fresh bucket starts from: one
/// dispatch at a time, until observations prove more is safe.
pub const DEFAULT_ALLOWANCE: f64 = 1.0;

/// The exponential retry base for the first rate-limit observation: the
/// first cooldown waits 60 seconds, the next 120, and so on.
pub const RETRY_BASE_SECS: u64 = 60;

/// The ceiling any exponential retry base saturates at, before the
/// deterministic 0-14 second [`jitter`] spread is added on top.
pub const RETRY_CEILING_SECS: u64 = 900;

/// The quiet interval between recoveries: once it has elapsed since the
/// last observation, exactly one slot is recovered and another full
/// interval is required for the next increment. Missed downtime is never
/// replayed.
pub const QUIET_RECOVERY_SECS: u64 = 300;

/// The grouping key of one model provider, derived only from the executed
/// model. A qualified model `zai/glm-5.3-flash` keys on its namespace (`zai`),
/// so every `zai/…` slug shares one bucket; a bare slug keys on itself and
/// can never collide with a qualified namespace of the same spelling
/// (`glm-5.3` vs `glm-5.3/…`). The prefix keeps the two spellings apart.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProviderKey(String);

impl ProviderKey {
    /// The provider key of the executed model `model`.
    #[must_use]
    pub fn from_model(model: &str) -> Self {
        match model.split_once('/') {
            Some((namespace, rest)) if !namespace.is_empty() && !rest.is_empty() => {
                Self(format!("ns:{namespace}"))
            }
            _ => Self(format!("slug:{model}")),
        }
    }

    /// The grouping key, stable across process runs: `ns:<namespace>` or
    /// `slug:<model>`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The namespace when the executed model was qualified, `None` for a
    /// bare slug.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.0.strip_prefix("ns:")
    }
}

impl std::fmt::Display for ProviderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where one provider's bucket lives: the current mailspace and pool. A
/// repository target is never part of the scope — the same model through the
/// same pool of the same mailspace is one bucket, whatever the job's target.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProviderScope {
    /// The mailspace key: the id from `.vivi/mailspace.id`, or the name slug
    /// when the config has none (the same key [`PoolId`] carries).
    pub mailspace: String,
    /// The pool name: the key of its `[pools.<name>]` table.
    pub pool: String,
}

impl ProviderScope {
    /// The scope of mailspace `mailspace` and pool `pool`.
    #[must_use]
    pub fn new(mailspace: impl Into<String>, pool: impl Into<String>) -> Self {
        Self {
            mailspace: mailspace.into(),
            pool: pool.into(),
        }
    }
}

impl From<&PoolId> for ProviderScope {
    fn from(id: &PoolId) -> Self {
        Self {
            mailspace: id.mailspace.clone(),
            pool: id.pool.clone(),
        }
    }
}

impl From<PoolId> for ProviderScope {
    fn from(id: PoolId) -> Self {
        Self {
            mailspace: id.mailspace,
            pool: id.pool,
        }
    }
}

impl std::fmt::Display for ProviderScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.mailspace, self.pool)
    }
}

/// One observation a bucket's allowance was learned from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderObservation {
    /// Epoch second the observation was recorded, `None` when unknown.
    #[serde(default)]
    pub at: Option<u64>,
    /// What was seen, as a class word (`refusal`, `success`, `limit`, ...).
    #[serde(default)]
    pub kind: String,
    /// One detail line from the observation, when there is one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Serializable state of one provider's bucket in one scope: what the pump
/// has learned about the provider's real limits. Everything defaults, so a
/// bucket written by an earlier build still loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderBucketState {
    /// Learned concurrent-dispatch allowance: how many dispatches this
    /// provider is currently believed to tolerate at once.
    #[serde(default = "default_allowance")]
    pub allowance: f64,
    /// The latest observation the allowance was learned from, if any.
    #[serde(default)]
    pub observation: Option<ProviderObservation>,
    /// Epoch second until which the provider is cooling down, when a
    /// rate-limit-shaped refusal put it there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<u64>,
    /// Epoch second at which recovery may next raise the allowance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recover_at: Option<u64>,
    /// How many consecutive rate-limit observations raised the retry
    /// delay, so each cooldown waits exponentially longer; later feedback
    /// on observed success may reset it.
    #[serde(default)]
    pub retry_step: u32,
}

fn default_allowance() -> f64 {
    DEFAULT_ALLOWANCE
}

impl Default for ProviderBucketState {
    fn default() -> Self {
        Self {
            allowance: DEFAULT_ALLOWANCE,
            observation: None,
            cooldown_until: None,
            recover_at: None,
            retry_step: 0,
        }
    }
}

impl ProviderBucketState {
    /// A fresh bucket at [`DEFAULT_ALLOWANCE`] with nothing observed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the bucket is cooling down at epoch second `now`.
    #[must_use]
    pub fn cooling(&self, now: u64) -> bool {
        self.cooldown_until.is_some_and(|until| now < until)
    }
}

/// Whether an observation with `kind` and `detail` is distinct from the
/// bucket's last recorded observation, and so halves the allowance when
/// recorded. All inputs are explicit so the later feedback caller can
/// deduplicate before building any record: only what was seen decides —
/// a repeat at a later epoch second is still the same observation.
#[must_use]
pub fn observation_is_distinct(
    previous: Option<&ProviderObservation>,
    kind: &str,
    detail: &str,
) -> bool {
    match previous {
        None => true,
        Some(prev) => prev.kind != kind || prev.detail != detail,
    }
}

/// The exponential base of the retry delay for the `step`-th consecutive
/// rate-limit observation: 60, 120, 240, ... seconds, saturating at the
/// 900-second base ceiling.
#[must_use]
fn retry_delay_base(step: u32) -> u64 {
    RETRY_BASE_SECS
        .saturating_mul(1u64 << step.saturating_sub(1).min(31))
        .min(RETRY_CEILING_SECS)
}

/// Record one authoritative rate-limit observation in `state` at epoch
/// second `at`, and derive the cooldown it earns. A distinct observation
/// (see [`observation_is_distinct`]) halves the allowance, rounding down,
/// floored at one only while `ceiling` enables capacity. Every
/// observation, distinct or repeated, resets the quiet-recovery interval
/// and re-earns a cooldown: the exponential base for the next consecutive
/// observation plus the deterministic 0-14 second [`jitter`] spread over
/// `seed`, extending — never shortening — the existing cooldown. Running
/// dispatches are never touched; only fresh starts are blocked while the
/// bucket cools. Returns whether the observation was distinct, so the
/// feedback caller can tell new learning from repetition.
#[must_use = "the distinctness of the recorded observation drives the caller's feedback"]
pub fn record_rate_limit(
    state: &mut ProviderBucketState,
    at: u64,
    kind: &str,
    detail: &str,
    seed: &str,
    ceiling: u32,
) -> bool {
    let distinct = observation_is_distinct(state.observation.as_ref(), kind, detail);
    if distinct {
        let floor = if ceiling == 0 { 0.0 } else { 1.0 };
        state.allowance = (state.allowance / 2.0).floor().max(floor);
    }
    state.retry_step = state.retry_step.saturating_add(1);
    let delay = retry_delay_base(state.retry_step).saturating_add(jitter(seed));
    let candidate = at.saturating_add(delay);
    state.cooldown_until = Some(
        state
            .cooldown_until
            .map_or(candidate, |until| until.max(candidate)),
    );
    state.recover_at = Some(at.saturating_add(QUIET_RECOVERY_SECS));
    state.observation = Some(ProviderObservation {
        at: Some(at),
        kind: kind.to_string(),
        detail: detail.to_string(),
    });
    distinct
}

/// Recover at most one learned slot once the quiet interval has elapsed:
/// exactly one slot per elapsed interval, the next interval counted from
/// `now` so missed downtime never replays, and never past `ceiling`. A
/// disabled provider (`ceiling` 0) never recovers. Returns whether a slot
/// was recovered.
#[must_use = "whether a slot was recovered decides the caller's next quiet check"]
pub fn recover_one_slot(state: &mut ProviderBucketState, now: u64, ceiling: u32) -> bool {
    if ceiling == 0 {
        return false;
    }
    let Some(quiet_at) = state.recover_at else {
        return false;
    };
    if now < quiet_at || state.allowance >= f64::from(ceiling) {
        return false;
    }
    state.allowance = (state.allowance + 1.0).min(f64::from(ceiling));
    state.recover_at = Some(now.saturating_add(QUIET_RECOVERY_SECS));
    true
}

/// Clamp the learned allowance against the current configured capacity
/// (the `slots`/`caps` a config reload brought): a tighter ceiling pulls
/// the allowance down, a looser one leaves learned reductions in place.
pub fn clamp_to_capacity(state: &mut ProviderBucketState, ceiling: u32) {
    state.allowance = state.allowance.min(f64::from(ceiling));
}

/// The allowance admission may actually use now: the learned allowance
/// clamped against the current configured capacity.
#[must_use]
pub fn effective_allowance(state: &ProviderBucketState, ceiling: u32) -> f64 {
    state.allowance.min(f64::from(ceiling))
}

/// Whether a fresh start may enter the bucket at epoch second `now`: not
/// while the provider cools — a cooldown blocks every fresh start — and
/// only while fewer than the effective allowance are in flight. Running
/// jobs are never cancelled by this policy; it gates fresh starts only.
#[must_use]
pub fn admits_fresh_start(
    state: &ProviderBucketState,
    now: u64,
    in_flight: u32,
    ceiling: u32,
) -> bool {
    !state.cooling(now) && f64::from(in_flight) < effective_allowance(state, ceiling)
}

/// One pool's providers: provider key -> bucket state. Serializable.
pub type ProviderBucketMap = BTreeMap<ProviderKey, ProviderBucketState>;

/// Every pool of a mailspace: pool name -> providers -> bucket state. A
/// plain nested map, so it survives a `serde_json` round trip unchanged.
pub type ProviderBuckets = BTreeMap<String, ProviderBucketMap>;

/// What admission knew about one provider when a decision was made.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdmissionProviderView {
    /// The ceiling derived from the pool config at that moment.
    pub ceiling: u32,
    /// Dispatches already in flight on this provider in this scope.
    pub in_flight: u32,
    /// The bucket's learned allowance at that moment.
    pub allowance: f64,
    /// Whether the bucket was cooling down at that moment.
    pub cooling: bool,
}

/// A frozen record of the per-provider facts one admission decision was made
/// from. Immutable by convention: it is built once and only cloned after.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmissionSnapshot {
    /// The scope the snapshot was taken in: mailspace and pool.
    pub scope: ProviderScope,
    /// Epoch second the snapshot was taken.
    pub taken_at: u64,
    /// Per provider: ceiling, in-flight count, allowance, cooling.
    pub providers: BTreeMap<ProviderKey, AdmissionProviderView>,
}

/// The largest number of concurrent dispatches the pool may admit at once
/// across the distinct models it offers. Pure: each uncapped offered model
/// contributes the pool's `slots`; explicit zero caps leave that model
/// disabled; the saturating sum is then bound by `slots`, so an empty or
/// fully disabled offering stays at zero however large the pool is.
pub fn configured_provider_ceiling(
    slots: u32,
    offered_models: &BTreeSet<String>,
    caps: &BTreeMap<String, u32>,
) -> u32 {
    bound(slots, offered_models.iter().map(String::as_str), caps)
}

/// Per-provider configured ceilings of one pool: provider key -> ceiling.
/// Offered harness models are grouped by [`ProviderKey`], each group counted
/// from its distinct models. Reads only `slots`, `harness.models` and `caps`:
/// `PoolConfig.provider` names the compute host, not a model provider, and
/// credentials are never consulted.
#[must_use]
pub fn configured_provider_ceilings(config: &PoolConfig) -> BTreeMap<ProviderKey, u32> {
    let empty = Vec::new();
    let models = config.harness.as_ref().map_or(&empty, |h| &h.models);
    let mut grouped: BTreeMap<ProviderKey, BTreeSet<String>> = BTreeMap::new();
    for model in models {
        grouped
            .entry(ProviderKey::from_model(model))
            .or_default()
            .insert(model.clone());
    }
    grouped
        .into_iter()
        .map(|(key, models)| {
            let ceiling = bound(
                config.slots,
                models.iter().map(String::as_str),
                &config.caps,
            );
            (key, ceiling)
        })
        .collect()
}

/// The shared counting rule: sum each distinct model's share — its explicit
/// cap, or `slots` when uncapped; zero caps skip — saturating, then bound by
/// `slots`.
fn bound<'a>(
    slots: u32,
    models: impl Iterator<Item = &'a str>,
    caps: &BTreeMap<String, u32>,
) -> u32 {
    if slots == 0 {
        return 0;
    }
    let mut total: u32 = 0;
    for model in models {
        let share = match caps.get(model) {
            Some(0) => continue,
            Some(cap) => *cap,
            None => slots,
        };
        total = total.saturating_add(share);
    }
    total.min(slots)
}
