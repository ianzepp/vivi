//! When a slice may start: the two scheduling rules from the
//! orchestration-reset README, as plain state the Lander keeps.
//!
//! - **Backpressure.** No slice starts while the Lander's backlog is longer
//!   than `max_backlog_ms` of landing work. A slice that has not started has
//!   no branch, so it cannot become backlog.
//! - **Hot-file mutex.** A slice whose write scope overlaps a configured hot
//!   path holds that path from admission until it lands or parks. Another
//!   slice that needs the same hot path waits.
//!
//! Requests are served first come, first served, except that one blocked by
//! the mutex does not hold up a later one that needs other paths.

use serde::{Deserialize, Serialize};

use crate::types::{AdmissionPolicy, AdmitRequest};

/// A granted slice and the hot paths it holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lease {
    pub key: String,
    pub hot: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Admission {
    pub waiting: Vec<AdmitRequest>,
    pub held: Vec<Lease>,
}

/// True when the backlog is over the policy's limit.
pub fn backpressured(policy: &AdmissionPolicy, backlog_units: usize) -> bool {
    if policy.max_backlog_ms == 0 {
        return false;
    }
    let units = u64::try_from(backlog_units).unwrap_or(u64::MAX);
    units.saturating_mul(policy.land_ms_per_unit) > policy.max_backlog_ms
}

/// Whether a write-scope path and a hot path name overlapping trees: one
/// is the other, or a directory prefix of it.
pub fn overlaps(path: &str, hot: &str) -> bool {
    let a = path.trim_end_matches('/');
    let b = hot.trim_end_matches('/');
    a == b || under(a, b) || under(b, a)
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The hot paths a write scope touches, in policy order.
pub fn hot_paths_of(policy: &AdmissionPolicy, scope: &[String]) -> Vec<String> {
    policy
        .hot_paths
        .iter()
        .filter(|hot| scope.iter().any(|p| overlaps(p, hot)))
        .cloned()
        .collect()
}

impl Admission {
    /// Queue a request. A slice that already holds a lease (a retried
    /// workflow task, say) is queued again and granted at once by `grant`.
    pub fn request(&mut self, request: AdmitRequest) {
        if !self.waiting.iter().any(|w| w.reply_to == request.reply_to) {
            self.waiting.push(request);
        }
    }

    /// Drop a slice's lease and any request it still has queued.
    pub fn release(&mut self, key: &str) {
        self.held.retain(|l| l.key != key);
        self.waiting.retain(|w| w.reply_to != key);
    }

    /// Grant every waiting request the rules allow now, take their leases,
    /// and return them in the order granted.
    pub fn grant(&mut self, policy: &AdmissionPolicy, backlog_units: usize) -> Vec<AdmitRequest> {
        let (granted, leases) = self.plan(policy, backlog_units);
        self.waiting
            .retain(|w| !granted.iter().any(|g| g.reply_to == w.reply_to));
        for lease in leases {
            self.held.retain(|l| l.key != lease.key);
            self.held.push(lease);
        }
        granted
    }

    /// Whether `grant` would grant anything now.
    pub fn has_grantable(&self, policy: &AdmissionPolicy, backlog_units: usize) -> bool {
        !self.plan(policy, backlog_units).0.is_empty()
    }

    fn plan(
        &self,
        policy: &AdmissionPolicy,
        backlog_units: usize,
    ) -> (Vec<AdmitRequest>, Vec<Lease>) {
        let mut granted = Vec::new();
        let mut leases: Vec<Lease> = Vec::new();
        for request in &self.waiting {
            let key = &request.reply_to;
            let renewal = self.held.iter().any(|l| &l.key == key);
            if !renewal && backpressured(policy, backlog_units) {
                continue;
            }
            let hot = hot_paths_of(policy, &request.write_scope);
            let taken = |h: &String| {
                self.held
                    .iter()
                    .chain(leases.iter())
                    .any(|l| &l.key != key && l.hot.contains(h))
            };
            if hot.iter().any(taken) {
                continue;
            }
            leases.push(Lease {
                key: key.clone(),
                hot,
            });
            granted.push(request.clone());
        }
        (granted, leases)
    }
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;
