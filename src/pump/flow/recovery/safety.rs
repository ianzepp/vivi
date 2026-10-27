//! Publication safety for a classified transient candidate: whether a
//! replacement may be allocated at all, or the failed job's own evidence
//! says its work already landed — or git cannot settle it.
//!
//! The gate runs after [`super::transient_class`] and before any replacement
//! request is frozen or minted. Evidence, in order of authority:
//!
//! 1. A privileged-runner publication receipt reporting a landed push owns
//!    the step, whatever the reply said: a genuinely empty final message
//!    after a real push is a lost answer, and provider markers beside a real
//!    push are diagnostics. Both return to normal collection, never a rerun.
//! 2. A runner fetch or checkout failure preceded execution: the agent never
//!    ran, so nothing can have landed; the rerun is safe.
//! 3. A read-only request cannot publish, so neither a receipt nor a git
//!    query is needed.
//! 4. An effectful request naming no publication ref cannot publish through
//!    the privileged runner (the runner pushes only the refs `publish_ref`
//!    names).
//! 5. Otherwise git decides from the request's own publication constraints —
//!    the same target ref, base and must-contain commit its collector's
//!    verification would read: verified absence alone permits the rerun, a
//!    verified landing does not, and unclear evidence keeps the existing
//!    side-effect-unclear path.
//!
//! Decisions carry diagnostic codes, never prose, and never a receipt: a
//! replacement is a fresh copy of the request, not a carrier of the failed
//! job's publication evidence.
#![cfg_attr(not(test), allow(dead_code))]

use super::RUNNER;
use crate::pump::flow::verify::{self, SideEffect, VerifyReq};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobOutcome, PoolJob};

/// The diagnostic codes a decision carries; stable words a caller alarms or
/// logs with its own wording.
pub(crate) mod code {
    /// The runner failed fetch or checkout before the agent could run.
    pub(crate) const PRE_EXECUTION: &str = "pre-execution";
    /// The request is read-only: it cannot have published anything.
    pub(crate) const READ_ONLY: &str = "read-only";
    /// The effectful request names no publication ref, so the privileged
    /// runner had nothing to publish.
    pub(crate) const NO_PUBLICATION_REF: &str = "no-publication-ref";
    /// Git verified the publication ref never took the job's work.
    pub(crate) const VERIFIED_ABSENCE: &str = "verified-absence";
    /// The privileged runner's receipt reports the push landed.
    pub(crate) const PUBLISHED_BY_RECEIPT: &str = "publish-receipt";
    /// Git verified the publication ref holds the job's work.
    pub(crate) const PUBLISHED_IN_GIT: &str = "git-verified";
    /// Git could not settle the remote: keep the side-effect-unclear path.
    pub(crate) const GIT_UNCLEAR: &str = "git-unclear";
}

/// Whether one classified transient candidate may be replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoverySafety {
    /// Allocate the replacement: repetition cannot duplicate a publication.
    Safe {
        /// Why repetition is safe; a [`code`] word.
        code: &'static str,
    },
    /// Never allocate: the work already landed, so normal collection owns
    /// the step's outcome.
    Published {
        /// Which evidence says it landed; a [`code`] word.
        code: &'static str,
    },
    /// Never allocate: git could not settle the remote, so the existing
    /// verification path (the side-effect-unclear hold) stays.
    Unclear {
        /// What could not be settled; a [`code`] word.
        code: &'static str,
    },
}

/// The publication-safety decision for the classified transient candidate
/// `job`; `class` is what [`super::transient_class`] answered for it. The
/// caller consults this before freezing or minting a replacement.
#[must_use]
pub(crate) fn decide(ctx: &Ctx, job: &PoolJob, class: &str) -> RecoverySafety {
    // A push the privileged runner reports as landed is the last word: the
    // step returns to normal collection whatever the reply or the provider
    // diagnostics said.
    if let Some(publish) = job.outcome.as_ref().and_then(JobOutcome::runner_publish)
        && publish.succeeded()
    {
        return RecoverySafety::Published {
            code: code::PUBLISHED_BY_RECEIPT,
        };
    }
    // A fetch or checkout failure preceded execution: the agent never ran.
    if class == RUNNER {
        return RecoverySafety::Safe {
            code: code::PRE_EXECUTION,
        };
    }
    // A read-only request publishes nothing.
    if job.read_only {
        return RecoverySafety::Safe {
            code: code::READ_ONLY,
        };
    }
    // An effectful request without a publication ref cannot publish through
    // the privileged runner.
    if job.publish_ref.is_none() {
        return RecoverySafety::Safe {
            code: code::NO_PUBLICATION_REF,
        };
    }
    // No decisive publication evidence: git decides from the request's own
    // constraints. Only a ref that verifiably did not take the work permits
    // the rerun.
    match verify::check(ctx, &publication_req(job)) {
        SideEffect::Absent => RecoverySafety::Safe {
            code: code::VERIFIED_ABSENCE,
        },
        SideEffect::Verified { .. } => RecoverySafety::Published {
            code: code::PUBLISHED_IN_GIT,
        },
        SideEffect::Unclear { .. } => RecoverySafety::Unclear {
            code: code::GIT_UNCLEAR,
        },
    }
}

/// The publication constraints of `job` as a verification request: the same
/// target ref, base and must-contain commit its collector's verification
/// reads, and no receipt — a successful one decided the question already,
/// and a failed one is not evidence.
fn publication_req(job: &PoolJob) -> VerifyReq {
    VerifyReq {
        target: job.target.clone().unwrap_or_default(),
        remote_ref: job.publish_ref.clone().unwrap_or_default(),
        before: job.from_ref.clone(),
        must_contain: job
            .publish_contains
            .clone()
            .or_else(|| job.publish_expect.clone())
            .unwrap_or_default(),
        receipt: None,
    }
}

#[cfg(test)]
#[path = "safety_test.rs"]
mod tests;
