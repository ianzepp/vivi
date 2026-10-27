//! The `[pools.<name>.ticket]` table: the settings only the `ticket` host has.
//!
//! A ticket pool has no machines of its own. A dispatch asks the vivi server
//! for a job ticket (`job_create`) and starts `vivi pool run` for it on an SSH
//! host, which pulls everything it needs from the server and pushes one
//! branch back. See `docs` of [`super::ticket`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::{bad, reject_unknown};
use crate::error::VivariumError;

/// Settings of the `ticket` machine host.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PoolTicket {
    /// What `ssh` is given to reach the worker host. Required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// URL the dispatcher itself uses to reach its vivi server. Required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Reach the server from the worker through `ssh -R` on the server's
    /// port (the dispatcher then needs no inbound port). Default true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<bool>,
    /// URL the worker uses for the server when `tunnel` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_server: Option<String>,
    /// The vivi binary on the worker host. Default `vivi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vivi: Option<String>,
    /// `VIVI_HOME` for the worker on the host (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_home: Option<String>,
    /// Project the jobs are for (id or hosted name). Default: the project
    /// whose mailspace holds this pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Target of that project. Default: its primary target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl PoolTicket {
    /// True unless the pool says `tunnel = false`.
    #[must_use]
    pub fn tunnel(&self) -> bool {
        self.tunnel.unwrap_or(true)
    }

    /// The worker binary as configured, or `vivi`.
    #[must_use]
    pub fn vivi_or_default(&self) -> &str {
        self.vivi.as_deref().unwrap_or("vivi")
    }

    /// Check the table of a `ticket` pool.
    ///
    /// # Errors
    /// Returns a configuration error naming the first problem.
    pub fn validate(&self, pool: &str) -> Result<(), VivariumError> {
        reject_unknown(pool, ".ticket", &self.unknown)?;
        for (field, value) in [("server", &self.server), ("worker_server", &self.worker_server)] {
            if let Some(url) = value
                && !(url.starts_with("http://") || url.starts_with("https://"))
            {
                return Err(bad(format!("[ticket] {field} must be an http(s) URL")));
            }
        }
        if !self.tunnel() && self.worker_server.is_none() {
            return Err(bad(
                "[ticket] worker_server is required when tunnel = false (the worker has no tunnel to the server)",
            ));
        }
        // These two land in the remote shell command line: plain path words
        // only (`~` and `$HOME` expand there).
        for (field, value) in [("vivi", &self.vivi), ("worker_home", &self.worker_home)] {
            if let Some(v) = value
                && (v.is_empty()
                    || !v
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_./~-$".contains(c)))
            {
                return Err(bad(format!(
                    "[ticket] {field} may hold only letters, digits and _ . / ~ - $ (got {v:?})"
                )));
            }
        }
        for (field, value) in [("host", &self.host)] {
            if let Some(v) = value
                && (v.trim().is_empty() || v.starts_with('-') || v.chars().any(char::is_control))
            {
                return Err(bad(format!("[ticket] {field} is not usable: {v:?}")));
            }
        }
        Ok(())
    }

    /// Report each setting a `ticket` pool needs and does not have.
    pub(super) fn missing(&self, need: &mut dyn FnMut(&str, &str)) {
        if self.host.as_deref().is_none_or(|h| h.trim().is_empty()) {
            need(
                "ticket.host",
                "[ticket] host is required (an ssh host or alias)",
            );
        }
        if self.server.as_deref().is_none_or(|s| s.trim().is_empty()) {
            need(
                "ticket.server",
                "[ticket] server is required (the URL this machine reaches its vivi server at)",
            );
        }
    }
}
