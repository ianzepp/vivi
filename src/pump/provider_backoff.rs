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

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::pool::config::PoolConfig;
use crate::pool::slot::PoolId;

/// The concurrent-dispatch allowance a fresh bucket starts from: one
/// dispatch at a time, until observations prove more is safe.
pub const DEFAULT_ALLOWANCE: f64 = 1.0;

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
