//! The worker config: one TOML file shared by every host. A worker started
//! with `--host <name>` polls only the queues `[hosts.<name>]` lists, with
//! the activity slot limit given for each. See `deploy.example.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::ConfigError;
use crate::types::{AdmissionPolicy, LanderInput, Routing, Tuning};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    pub routing: RoutingConfig,
    pub hosts: BTreeMap<String, HostConfig>,
    pub targets: BTreeMap<String, Target>,
    pub tuning: Tuning,
    #[serde(default)]
    pub lander: LanderConfig,
    #[serde(default)]
    pub record: RecordConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// `http://host:7233`. Use the LAN IP where `.local` lookups are slow.
    pub address: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
}

fn default_namespace() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    #[serde(default = "q_control")]
    pub control: String,
    #[serde(default = "q_lane")]
    pub lane: String,
    #[serde(default = "q_agent")]
    pub agent: String,
    #[serde(default = "q_check")]
    pub check: String,
    #[serde(default = "q_agent")]
    pub review: String,
    #[serde(default = "q_gate")]
    pub gate: String,
    #[serde(default = "q_agent")]
    pub plan: String,
    pub agent_hosts: Vec<String>,
    /// Review on its own hosts (an LLM-only box). Unset: the agent hosts.
    #[serde(default)]
    pub review_hosts: Vec<String>,
    /// Plan on its own hosts. Unset: plan runs on the control queue.
    #[serde(default)]
    pub plan_hosts: Vec<String>,
    pub gate_hosts: Vec<String>,
    #[serde(default = "default_affinity")]
    pub affinity_wait_s: u64,
    #[serde(default = "default_fallback")]
    pub fallback_wait_s: u64,
}

fn q_control() -> String {
    "control".to_string()
}
fn q_lane() -> String {
    "lane".to_string()
}
fn q_agent() -> String {
    "agent-{host}".to_string()
}
fn q_check() -> String {
    "check-{host}".to_string()
}
fn q_gate() -> String {
    "gate-{host}".to_string()
}
fn default_affinity() -> u64 {
    60
}
fn default_fallback() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Queue name → most activities this host runs from it at once.
    pub queues: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// The lane: `git@hub.local:<owner>/<repo>.git`.
    pub remote: String,
    /// The branch the Lander publishes to.
    pub main: String,
    #[serde(default = "default_prefix")]
    pub branch_prefix: String,
    /// The pre-queue check, run with `sh -c` in the check tree on the slice
    /// head merged with current main. `VIVI_BASE` and `VIVI_HEAD` are set.
    pub check_command: String,
    #[serde(default = "default_check_timeout")]
    pub check_timeout_s: u64,
    pub agent: AgentConfig,
    pub lander: LanderPaths,
}

fn default_prefix() -> String {
    "vivi/".to_string()
}
fn default_check_timeout() -> u64 {
    900
}

/// Where the agent hosts keep this target. `~` is the worker's home.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Project root of the local mailspace that owns the pool.
    pub space: String,
    pub pool: String,
    /// The pool's `worktree_of` repository: the warm tree.
    pub repo: String,
    /// A clone the check runs in.
    pub check_tree: String,
    pub model: String,
    #[serde(default = "default_budget")]
    pub budget_s: u64,
    #[serde(default = "default_vivi")]
    pub vivi: String,
    #[serde(default = "default_from")]
    pub from: String,
    #[serde(default = "default_to")]
    pub to: String,
}

fn default_budget() -> u64 {
    1_800
}
fn default_vivi() -> String {
    "vivi".to_string()
}
fn default_from() -> String {
    "operator".to_string()
}
fn default_to() -> String {
    "hand".to_string()
}

/// The Lander's commands and trees for this target.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanderPaths {
    /// `lab/lander/compose`.
    pub compose: String,
    /// `lab/lander/gate`.
    pub gate: String,
    /// A bare clone of the lane on the lane host; compose works in it.
    pub cache: String,
    /// The warm gate tree on the gate hosts; it has a `lane` remote.
    pub gate_tree: String,
    #[serde(default = "default_gate_timeout")]
    pub gate_timeout_s: u64,
}

fn default_gate_timeout() -> u64 {
    1_800
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LanderConfig {
    pub batch_window_s: u64,
    pub max_batch: usize,
    pub step_cap_s: u64,
    pub gate_cap_s: u64,
    pub hot_paths: Vec<String>,
    /// About an hour of landing capacity, per the README.
    pub max_backlog_s: u64,
    /// Measured: about 3 minutes a unit with batch-and-bisect.
    pub land_s_per_unit: u64,
}

impl Default for LanderConfig {
    fn default() -> Self {
        LanderConfig {
            batch_window_s: 60,
            max_batch: 16,
            step_cap_s: 300,
            gate_cap_s: 2_400,
            hot_paths: Vec::new(),
            max_backlog_s: 3_600,
            land_s_per_unit: 180,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecordKind {
    /// Keep calls in memory (tests).
    Memory,
    /// Print each call to the control worker's log.
    #[default]
    Log,
    /// Write to the vivi server through `vivi mcp call`.
    Mcp,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct RecordConfig {
    pub kind: RecordKind,
    /// `http://hub.local:18190`.
    pub server: String,
    /// Project id or hosted project name.
    pub project: String,
    /// File holding the MCP API key; read when a call is made.
    pub key_file: String,
    /// The role the record writes as (`for` on mark and done).
    pub role: String,
    pub vivi: String,
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn load(path: &str) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_string(),
            source,
        })?;
        Config::parse(&text)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |msg: String| Err(ConfigError::Invalid(msg));
        let hosts = self
            .routing
            .agent_hosts
            .iter()
            .chain(&self.routing.review_hosts)
            .chain(&self.routing.plan_hosts)
            .chain(&self.routing.gate_hosts);
        for host in hosts {
            if !self.hosts.contains_key(host) {
                return bad(format!(
                    "routing names host {host:?}, which has no [hosts] entry"
                ));
            }
        }
        for (name, host) in &self.hosts {
            if host.queues.is_empty() {
                return bad(format!("[hosts.{name}] serves no queue"));
            }
            if let Some((q, _)) = host.queues.iter().find(|(_, n)| **n == 0) {
                return bad(format!("[hosts.{name}] queue {q:?} has 0 slots"));
            }
        }
        if !self
            .hosts
            .values()
            .any(|h| h.queues.contains_key(&self.routing.control))
        {
            return bad(format!(
                "no host serves the control queue {:?}",
                self.routing.control
            ));
        }
        if self.record.kind == RecordKind::Mcp
            && (self.record.server.is_empty() || self.record.project.is_empty())
        {
            return bad("[record] kind = \"mcp\" needs server and project".to_string());
        }
        Ok(())
    }

    /// The queues a host polls and the activity slots for each.
    pub fn queues_for(&self, host: &str) -> Result<Vec<(String, usize)>, ConfigError> {
        let entry = self
            .hosts
            .get(host)
            .ok_or_else(|| ConfigError::Invalid(format!("no [hosts.{host}] entry")))?;
        Ok(entry.queues.iter().map(|(q, n)| (q.clone(), *n)).collect())
    }

    pub fn target(&self, name: &str) -> Result<&Target, ConfigError> {
        self.targets
            .get(name)
            .ok_or_else(|| ConfigError::Invalid(format!("no [targets.{name}] entry")))
    }

    pub fn routing(&self) -> Routing {
        let r = &self.routing;
        Routing {
            control: r.control.clone(),
            lane: r.lane.clone(),
            agent: r.agent.clone(),
            check: r.check.clone(),
            review: r.review.clone(),
            gate: r.gate.clone(),
            plan: r.plan.clone(),
            agent_hosts: r.agent_hosts.clone(),
            review_hosts: r.review_hosts.clone(),
            plan_hosts: r.plan_hosts.clone(),
            gate_hosts: r.gate_hosts.clone(),
            affinity_wait_ms: r.affinity_wait_s.saturating_mul(1_000),
            fallback_wait_ms: r.fallback_wait_s.saturating_mul(1_000),
        }
    }

    pub fn lander_input(&self, target: &str) -> Result<LanderInput, ConfigError> {
        self.target(target)?;
        let l = &self.lander;
        Ok(LanderInput {
            batch_window_ms: l.batch_window_s.saturating_mul(1_000),
            max_batch: l.max_batch,
            step_cap_ms: l.step_cap_s.saturating_mul(1_000),
            gate_cap_ms: l.gate_cap_s.saturating_mul(1_000),
            target: target.to_string(),
            routing: self.routing(),
            admission: AdmissionPolicy {
                hot_paths: l.hot_paths.clone(),
                max_backlog_ms: l.max_backlog_s.saturating_mul(1_000),
                land_ms_per_unit: l.land_s_per_unit.saturating_mul(1_000),
            },
        })
    }
}

/// The Lander's workflow id for a target.
pub fn lander_id(target: &str) -> String {
    format!("lander-{target}")
}

/// Expand a leading `~/` to the home directory.
pub fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
