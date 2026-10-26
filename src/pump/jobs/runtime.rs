//! Serializable execution metadata for durable pool jobs.
//!
//! [`Runtime`] is the per-execution half of a [`PoolJob`]: which physical
//! execution the job is on, what actually ran (pool, model, provider, start
//! ref/commit/time), its typed result evidence and processed-result
//! identity, rate-limit and scheduled-wait bookkeeping, archived
//! rate-limited executions, and an optional continuation. It deserializes
//! at its default for legacy rows and never takes part in `same_spec`
//! identity, so re-enqueueing identical work stays a no-op however far the
//! execution metadata has drifted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{JobOutcome, JobStore};

/// Physical execution metadata for a durable pool job.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Runtime {
    /// Current physical execution ID; `None` until one is minted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// Pool that actually accepted the current execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Model the current execution actually runs with, after any role
    /// fallback or override was resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider backing [`Runtime::model`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Start ref the current execution checked out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ref: Option<String>,
    /// Full commit `start_ref` pointed at when the execution started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_commit: Option<String>,
    /// Execution start time in epoch seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    /// Typed result evidence once the execution reaches a terminal state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ExecutionEvidence>,
    /// Identity of the processed terminal result, once settled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processed: Option<ProcessedResult>,
    /// Rate-limit retry bookkeeping for the current execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitState>,
    /// Scheduled wait parked on the job, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_wait: Option<ScheduledWait>,
    /// Archived rate-limited executions, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub archived: Vec<ArchivedExecution>,
    /// Continuation this execution resumes from, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,
}

impl Runtime {
    /// Whether every runtime field is still at its default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Typed terminal result evidence for one physical execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ExecutionEvidence {
    /// The pool reported the execution finished cleanly.
    Finished {
        /// Short porcelain detail line from the pool.
        detail: String,
        /// Terminal time in epoch seconds.
        at: u64,
    },
    /// The pool stopped the execution at its time budget.
    Timeout {
        /// Short porcelain detail line from the pool.
        detail: String,
        /// Terminal time in epoch seconds.
        at: u64,
    },
    /// The pool lost the slot or the result.
    Lost {
        /// Short porcelain detail line from the pool.
        detail: String,
        /// Terminal time in epoch seconds.
        at: u64,
    },
}

impl ExecutionEvidence {
    /// Porcelain state label matching the reconcile vocabulary.
    #[must_use]
    pub fn state(&self) -> &'static str {
        match self {
            Self::Finished { .. } => "finished",
            Self::Timeout { .. } => "timeout",
            Self::Lost { .. } => "lost",
        }
    }

    /// The porcelain detail line carried by the evidence.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Finished { detail, .. }
            | Self::Timeout { detail, .. }
            | Self::Lost { detail, .. } => detail,
        }
    }

    /// Terminal time in epoch seconds.
    #[must_use]
    pub fn at(&self) -> u64 {
        match self {
            Self::Finished { at, .. } | Self::Timeout { at, .. } | Self::Lost { at, .. } => *at,
        }
    }
}

/// Identity of the processed terminal result of one physical execution.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessedResult {
    /// Execution whose result was processed.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub execution_id: String,
    /// Stable digest of the processed outcome content.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub outcome_digest: String,
    /// Processing time in epoch seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processed_at: Option<u64>,
}

/// Rate-limit retry bookkeeping for the current physical execution.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitState {
    /// Rate-limit retries this execution has already consumed.
    #[serde(skip_serializing_if = "is_zero")]
    pub retry_count: u32,
    /// Earliest epoch-second time the next retry may start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_deadline: Option<u64>,
}

/// A scheduled wait parked on a job before its next dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScheduledWait {
    /// Epoch seconds the wait was scheduled at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<u64>,
    /// Epoch seconds the wait ends and dispatch may resume.
    pub wake_at: u64,
}

/// An archived rate-limited execution with its attribution and terminal
/// outcome.
///
/// An archive holds everything needed to restore the original terminal
/// result and settle it through the normal path.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchivedExecution {
    /// Physical execution that was rate-limited.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub execution_id: String,
    /// Pool that accepted the execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Model the execution ran with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider backing the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Mailspace task the execution owned, if one had been sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Execution start time in epoch seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    /// Epoch seconds the provider rate limit was hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limited_at: Option<u64>,
    /// Typed terminal outcome of the archived execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ExecutionEvidence>,
    /// Terminal receipt saved with the outcome; empty when the pool
    /// reported none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub receipt: String,
}

impl ArchivedExecution {
    /// Restore the original terminal result for normal settlement: the
    /// porcelain state, pool detail line, saved receipt and terminal time.
    /// Returns `None` while the archive carries no terminal outcome.
    #[must_use]
    pub fn restore_outcome(&self) -> Option<JobOutcome> {
        let outcome = self.outcome.as_ref()?;
        Some(JobOutcome {
            state: outcome.state().to_owned(),
            detail: outcome.detail().to_owned(),
            receipt: self.receipt.clone(),
            at: outcome.at(),
        })
    }
}

/// A continuation an execution resumes from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Continuation {
    /// Ref naming the continuation state.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub from_ref: String,
    /// Full commit `from_ref` pointed at when recorded.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub from_commit: String,
    /// Epoch seconds the continuation was recorded at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<u64>,
}

/// Effective provider rate limit observed for one pool or provider name.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderLimit {
    /// Effective limit per window; `None` while unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_limit: Option<u32>,
    /// Epoch seconds of the latest observation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<u64>,
    /// Epoch seconds the current cooldown ends.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<u64>,
    /// Epoch seconds at which the limit is expected to recover.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_at: Option<u64>,
    /// Saturating backoff steps already taken, in seconds, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub backoff_history: Vec<u64>,
}

impl ProviderLimit {
    /// Seconds the next backoff step would wait: `base_seconds` first,
    /// then the previous step doubled, always capped at `max_seconds`.
    #[must_use]
    pub fn next_backoff(&self, base_seconds: u64, max_seconds: u64) -> u64 {
        self.backoff_history
            .last()
            .map_or(base_seconds, |last| last.saturating_mul(2))
            .min(max_seconds)
    }

    /// Record one saturating backoff step under [`ProviderLimit::
    /// next_backoff`] policy, keeping at most `history_cap` entries.
    pub fn step_backoff(&mut self, base_seconds: u64, max_seconds: u64, history_cap: usize) {
        let next = self.next_backoff(base_seconds, max_seconds);
        self.backoff_history.push(next);
        if self.backoff_history.len() > history_cap {
            self.backoff_history
                .drain(..self.backoff_history.len() - history_cap);
        }
    }
}

/// Nested effective rate limits per pool and per provider.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderLimits {
    /// Effective limits keyed per pool name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub pools: BTreeMap<String, ProviderLimit>,
    /// Effective limits keyed per provider name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, ProviderLimit>,
}

impl ProviderLimits {
    /// Effective limit for `pool`, if one was observed.
    #[must_use]
    pub fn pool(&self, pool: &str) -> Option<&ProviderLimit> {
        self.pools.get(pool)
    }

    /// Effective limit for `provider`, if one was observed.
    #[must_use]
    pub fn provider(&self, provider: &str) -> Option<&ProviderLimit> {
        self.providers.get(provider)
    }

    /// Mutable entry for `pool`, created at its default when absent.
    pub fn pool_mut(&mut self, pool: &str) -> &mut ProviderLimit {
        self.pools.entry(pool.to_owned()).or_default()
    }

    /// Mutable entry for `provider`, created at its default when absent.
    pub fn provider_mut(&mut self, provider: &str) -> &mut ProviderLimit {
        self.providers.entry(provider.to_owned()).or_default()
    }
}

/// Narrow dispatch interface: a [`JobStore`] that also exposes the
/// observed effective provider limits.
///
/// Dispatch admission may read these limits to refuse pools or providers
/// under an active cooldown, and a dispatch decision may record a fresh
/// observation; no other path touches them.
pub trait DispatchStore: JobStore {
    /// Observed limits keyed per pool and per provider name.
    fn provider_limits(&self) -> &ProviderLimits;

    /// Mutable limits for recording an observation or cooldown.
    fn provider_limits_mut(&mut self) -> &mut ProviderLimits;
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
