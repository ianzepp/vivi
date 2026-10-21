//! The `[pools.<name>.worker]` table: the settings only the `worker` host
//! has.
//!
//! A worker is a machine reached over SSH that offers capacity (memory for
//! RAM disks, disk for quota images, plain directories) and builds one
//! bounded workspace per job. The pool names the boundary its jobs need;
//! the worker's own `worker.conf` holds its budgets, so a request it cannot
//! hold now is refused and the pump tries the next pool.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::{bad, reject_unknown};
use crate::error::VivariumError;

/// The worker root when none is configured: the login user's home, on the
/// internal disk (macOS will not mount a volume inside another external one).
pub const DEFAULT_WORKER_ROOT: &str = "~/vivi-worker";

/// The boundary kinds a worker driver builds.
pub const WORKER_KINDS: [&str; 3] = ["apfs", "ram", "dir"];

/// Settings of the `worker` machine host.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PoolWorker {
    /// What `ssh` is given to reach the worker: `user@server`, or a `Host`
    /// alias. Required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The worker root holding `worker.conf`, the ledger and the slots.
    /// Default: `~/vivi-worker`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// The boundary each job gets: `apfs` (a quota-limited disk image),
    /// `ram` (a RAM disk) or `dir` (a plain directory). Required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The boundary's size in GiB; required for `apfs` and `ram`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_gib: Option<u32>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl PoolWorker {
    /// The worker root as configured, or the default.
    #[must_use]
    pub fn root_or_default(&self) -> &str {
        self.root.as_deref().unwrap_or(DEFAULT_WORKER_ROOT)
    }

    /// The request a `create` carries to the driver: `kind=<k> size_gib=<n>`.
    #[must_use]
    pub fn request(&self) -> String {
        format!(
            "kind={} size_gib={}",
            self.kind.as_deref().unwrap_or_default(),
            self.size_gib.unwrap_or(0)
        )
    }

    /// Check the table of a `worker` pool.
    ///
    /// # Errors
    /// Returns a configuration error naming the first problem.
    pub fn validate(&self, pool: &str) -> Result<(), VivariumError> {
        reject_unknown(pool, ".worker", &self.unknown)?;
        if self.host.as_deref().is_none_or(|h| h.trim().is_empty()) {
            return Err(bad("[worker] host is required (an ssh host or alias)"));
        }
        let kind = self.kind.as_deref().unwrap_or_default();
        if !WORKER_KINDS.contains(&kind) {
            return Err(bad(format!(
                "[worker] kind must be one of {}, got {kind:?}",
                WORKER_KINDS.join(", ")
            )));
        }
        match (kind, self.size_gib) {
            ("apfs" | "ram", None | Some(0)) => Err(bad(format!(
                "[worker] size_gib is required for kind {kind} (whole GiB, at least 1)"
            ))),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "config_worker_test.rs"]
mod tests;
