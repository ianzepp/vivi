//! Rebuild a durable job's spec for a rerun or a deterministic copy.
//!
//! [`PoolJob::rerun_spec`] is the one writer of "copy every execution input
//! into a fresh [`JobSpec`]": reruns of a broken step job and reslice copies
//! of a step both go through it, so neither can silently drop a field.

use crate::pump::jobs::{JobSpec, PoolJob};

impl PoolJob {
    /// Copy every execution and input field of this job into a fresh
    /// [`JobSpec`] named `new_id`.
    ///
    /// Runtime ownership and result fields — pool, timestamps, status,
    /// outcome, applied-at, refusal counters and held state — have no spec
    /// identity: the copy enqueues like any new pending job.
    #[must_use]
    pub(crate) fn rerun_spec(&self, new_id: impl Into<String>) -> JobSpec {
        JobSpec {
            id: new_id.into(),
            kind: self.kind.clone(),
            role: self.role.clone(),
            subject: self.subject.clone(),
            body: self.body.clone(),
            pool_candidates: self.pool_candidates.clone(),
            local_only: self.local_only,
            pool_exclude: self.pool_exclude.clone(),
            target: self.target.clone(),
            extra_targets: self.extra_targets.clone(),
            from_ref: self.from_ref.clone(),
            with: self.with.clone(),
            model: self.model.clone(),
            command: self.command.clone(),
            builtin_role: self.builtin_role.clone(),
            branch: self.branch.clone(),
            allow_default_branch: self.allow_default_branch,
            publish_ref: self.publish_ref.clone(),
            publish_expect: self.publish_expect.clone(),
            publish_contains: self.publish_contains.clone(),
            delete_ref: self.delete_ref.clone(),
            delete_expect: self.delete_expect.clone(),
            budget: self.budget,
            read_only: self.read_only,
            units: self.units.clone(),
            source_jobs: self.source_jobs.clone(),
            payload: self.payload.clone(),
            task: self.task.clone(),
        }
    }
}

#[cfg(test)]
#[path = "retry_test.rs"]
mod tests;
