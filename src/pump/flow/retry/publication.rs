//! The shared publication safety gate: whether a finished step job's git
//! side effect already landed, consulted before any replay of the job — a
//! transient retry now, held-step resumption later.
//!
//! Evidence is machine evidence only: the runner's privileged publish
//! receipt and git, never the hand's own message. For a job that publishes,
//! a decisive successful receipt for the job's own ref wins outright;
//! otherwise [`verify::check`] keeps the git distinction its owners collect
//! by — verified work advances through its owning publication path, an
//! absent effect permits replay, and an unclear effect stays held with
//! `flow-side-effect-unclear`. A reply, an envelope correction or a retry
//! allowance is never permission to replay an uncertain merge: only a
//! receipt, git ancestry and the ref's before state decide, and the source
//! fetch and publication ancestry checks of `verify` are reused whole.

use crate::pump::flow::verify::{self, SideEffect, VerifyReq};
use crate::pump::host::Ctx;
use crate::pump::jobs::{PoolJob, RunnerPublishReceipt};

/// What settled a [`Publication::Published`] answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Evidence {
    /// The runner's own successful publish receipt for the job's ref.
    Receipt,
    /// Git verified the ref against the job's before state or ancestry.
    Git,
}

/// What the gate saw about a finished job's published side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Publication {
    /// The job publishes nothing: no verification ran, and a replay is the
    /// ordinary retry policy's decision.
    None,
    /// The work landed and must not execute again. `sha` is the published
    /// or current ref SHA; the ref is the job's own `publish_ref`. A
    /// published hand proceeds to its slice merge, a repairer to its repair
    /// verification, and an actual merge to completion — without running
    /// the work again and without an envelope-fix request for missing
    /// final prose.
    Published { sha: String, via: Evidence },
    /// The ref did not move: the job demonstrably published nothing, so a
    /// replay cannot duplicate work.
    Absent,
    /// Git could not settle it, or the evidence conflicts: hold with
    /// `flow-side-effect-unclear`. No receipt word, reply or retry budget
    /// overrides this.
    Unclear { reason: String },
}

/// Gate one finished job's publication before any replay of it. A job with
/// no publish ref is not a publication question at all. A publishing job is
/// settled from its runner receipt when that receipt decisively succeeded
/// on the job's own ref, else from git through [`verify::check`]; the
/// receipt's candidate SHA anchors the ancestry check when the job carries
/// no `publish_contains` / `publish_expect` of its own.
#[must_use]
pub(crate) fn gate(ctx: &Ctx, job: &PoolJob) -> Publication {
    let Some(remote_ref) = job.publish_ref.clone() else {
        return Publication::None;
    };
    let Some(target) = job.target.clone() else {
        return Publication::Unclear {
            reason: format!("job {} publishes {remote_ref} but names no target", job.id),
        };
    };
    if let Some(publish) = decisive(job) {
        return Publication::Published {
            sha: publish.remote_sha,
            via: Evidence::Receipt,
        };
    }
    let Some(must_contain) = contains(job) else {
        return Publication::Unclear {
            reason: format!(
                "job {} publishes {remote_ref} with no receipt and no commit to verify against",
                job.id
            ),
        };
    };
    match verify::check(
        ctx,
        &VerifyReq {
            target,
            remote_ref,
            before: job.from_ref.clone(),
            must_contain,
            // A receipt that did not decisively succeed is not evidence the
            // ref moved: git alone decides, and a moved ref without the
            // anchored commit stays the conflict it is.
            receipt: None,
        },
    ) {
        SideEffect::Verified { sha } => Publication::Published {
            sha,
            via: Evidence::Git,
        },
        SideEffect::Absent => Publication::Absent,
        SideEffect::Unclear { reason } => Publication::Unclear { reason },
    }
}

/// The decisive publish receipt of `job`: the runner's own `pushed` or
/// `already-on-remote` word for the job's own ref, with the remote SHA it
/// names. A receipt naming another ref never decides this job, so one
/// repair attempt's success cannot release a replay of another attempt's
/// work.
fn decisive(job: &PoolJob) -> Option<RunnerPublishReceipt> {
    let publish = job.outcome.as_ref()?.runner_publish()?;
    let wanted = job.publish_ref.as_deref()?;
    (publish.succeeded() && publish.reference == wanted).then_some(publish)
}

/// The commit `job`'s publish must contain: the job's own ancestry anchor,
/// else the candidate the runner named on a receipt that did not succeed,
/// so git can still separate a landed publish from a moved ref. `None`
/// leaves nothing to verify and the gate holds.
fn contains(job: &PoolJob) -> Option<String> {
    job.publish_contains
        .clone()
        .or_else(|| job.publish_expect.clone())
        .or_else(|| {
            job.outcome
                .as_ref()?
                .runner_publish()
                .map(|publish| publish.candidate)
        })
}

#[cfg(test)]
#[path = "publication_test.rs"]
mod tests;
