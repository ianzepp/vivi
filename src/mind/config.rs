//! `mind.toml`: the mind's authority stage, wake heartbeat, and paths.
//!
//! The file sits next to the host registry, at
//! `<config_dir>/vivi/mind.toml` (`VIVI_CONFIG_DIR` overrides
//! `<config_dir>`, as for the registry). Every key is optional; a missing
//! file is the default config, which is the `observe` stage.
//!
//! ```toml
//! authority = "observe"          # observe | unstick | file-fixes
//! heartbeat = "30m"
//! proxy_url = "http://127.0.0.1:18180"
//! pi_command = "pi"
//! human_only = ["spending-accounts", "credentials", "host-root",
//!               "releases", "product-intent"]
//! # model = "kimi/kimi-k2"       # passed to pi; resolved through the proxy
//! # socket = "/run/vivi/mind.sock"
//! # session_dir = "/var/lib/vivi/mind/sessions"
//!
//! [budgets]
//! per_need_actions = 3           # unstick actions per need per window
//! per_need_window = "24h"
//! max_wakes_per_hour = 6
//! ```
//!
//! The mind holds no model keys: models are reached only through the hub's
//! loopback proxy, and a non-loopback `proxy_url` is refused.

use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::VivariumError;
use crate::duration::parse_duration_secs;

/// Config file name, next to `pump-service.toml`.
pub const CONFIG_FILENAME: &str = "mind.toml";
/// Default wake heartbeat.
pub const DEFAULT_HEARTBEAT: &str = "30m";
/// The hub proxy's default loopback front door (see `proxy::config`).
pub const DEFAULT_PROXY_URL: &str = "http://127.0.0.1:18180";
/// The pi harness executable.
pub const DEFAULT_PI_COMMAND: &str = "pi";
/// Environment override for the mind home (state, socket, sessions, log).
pub const ENV_MIND_HOME: &str = "VIVI_MIND_HOME";
/// Categories the mind hands to a human before acting on them, in default
/// order. An explicit `human_only` list replaces these wholesale.
pub const DEFAULT_HUMAN_ONLY: [&str; 5] = [
    "spending-accounts",
    "credentials",
    "host-root",
    "releases",
    "product-intent",
];

/// How much the mind may do. Each stage is opened deliberately by the
/// operator in `mind.toml`; a stage includes every power below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorityStage {
    /// Read-only status and logs; letters to the operator.
    Observe,
    /// Requeue, replan, answer and unlock within per-need budgets; pause a
    /// pump and escalate.
    Unstick,
    /// File fix needs to the vivi project through the normal pump, stopping
    /// at merged.
    FileFixes,
    /// `vivi update --version` after the operator cuts a release. Not
    /// implemented; the config loader refuses it.
    UpgradeOnApproval,
    /// Cut releases itself. Long term only; the config loader refuses it.
    SelfRelease,
}

impl AuthorityStage {
    /// The kebab-case name, as written in `mind.toml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Unstick => "unstick",
            Self::FileFixes => "file-fixes",
            Self::UpgradeOnApproval => "upgrade-on-approval",
            Self::SelfRelease => "self-release",
        }
    }

    /// Whether this build implements the stage. Later stages exist so the
    /// config shape is settled, but opening them is refused.
    #[must_use]
    pub fn implemented(self) -> bool {
        matches!(self, Self::Observe | Self::Unstick | Self::FileFixes)
    }
}

/// Per-need action budgets and the wake rate cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budgets {
    /// Unstick actions allowed on one need inside `per_need_window_secs`.
    pub per_need_actions: u32,
    /// Sliding window for `per_need_actions`.
    pub per_need_window_secs: u64,
    /// Autonomous wakes allowed in any rolling hour.
    pub max_wakes_per_hour: u32,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            per_need_actions: 3,
            per_need_window_secs: 86_400,
            max_wakes_per_hour: 6,
        }
    }
}

/// One `human_only` category: a trusted policy token from `mind.toml`,
/// never inferred from need text. A lowercase kebab-case word such as
/// `spending-accounts`; anything else is refused at parse time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[serde(try_from = "String")]
pub struct HumanOnlyCategory(String);

impl HumanOnlyCategory {
    /// The token as written in `mind.toml`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for HumanOnlyCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for HumanOnlyCategory {
    type Error = VivariumError;

    fn try_from(token: String) -> Result<Self, VivariumError> {
        if valid_category(&token) {
            Ok(Self(token))
        } else {
            Err(VivariumError::Config(format!(
                "human_only category {token:?} must be a lowercase kebab-case token"
            )))
        }
    }
}

/// Accept lowercase kebab-case: an ASCII lowercase letter, then lowercase
/// letters or digits joined by single interior hyphens. Refuses blank,
/// uppercase, spaced, and bare-hyphen tokens.
fn valid_category(token: &str) -> bool {
    let mut chars = token.chars();
    if !matches!(chars.next(), Some(first) if first.is_ascii_lowercase()) {
        return false;
    }
    let mut after_hyphen = false;
    for c in chars {
        match c {
            '-' if !after_hyphen => after_hyphen = true,
            c if c.is_ascii_lowercase() || c.is_ascii_digit() => after_hyphen = false,
            _ => return false,
        }
    }
    !after_hyphen
}

/// The default human-only categories, in policy order.
#[must_use]
pub fn human_only_defaults() -> Vec<HumanOnlyCategory> {
    DEFAULT_HUMAN_ONLY
        .iter()
        .map(|token| HumanOnlyCategory((*token).to_string()))
        .collect()
}

/// Resolve the human-only policy: the written list when present, else the
/// defaults. An explicit empty list means no human escalation. Duplicates
/// are refused so each category appears once.
fn human_only_policy(
    written: Option<Vec<HumanOnlyCategory>>,
) -> Result<Vec<HumanOnlyCategory>, VivariumError> {
    let list = written.unwrap_or_else(human_only_defaults);
    let mut seen = HashSet::new();
    for category in &list {
        if !seen.insert(category.as_str()) {
            return Err(VivariumError::Config(format!(
                "human_only lists {} more than once",
                category.as_str()
            )));
        }
    }
    Ok(list)
}

/// The resolved mind config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MindConfig {
    pub authority: AuthorityStage,
    /// Heartbeat as written (`30m`), for the role schedule helpers.
    pub heartbeat: String,
    pub heartbeat_secs: u64,
    pub proxy_url: String,
    pub pi_command: String,
    pub model: Option<String>,
    pub budgets: Budgets,
    /// Categories to hand to a human before acting on them: the written
    /// `human_only` list, or the defaults when absent; empty means none.
    pub human_only: Vec<HumanOnlyCategory>,
    /// The vivi-server the mind acts through; `None` leaves every permitted
    /// action a dry run.
    pub mcp: Option<McpConfig>,
    pub paths: MindPaths,
}

/// How the mind reaches the server that runs the pumps. The mind acts the
/// way any MCP client does: it calls the server's tools with its own key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfig {
    /// Server base URL, for example `http://127.0.0.1:18190`.
    pub server: String,
    /// File holding the mind's server key.
    pub key_file: PathBuf,
    /// Follow-up turns after one that ran actions, so the session sees the
    /// results and can check its work. `0` ends the wake after the first
    /// turn.
    pub follow_ups: u32,
}

/// Longest follow-up chain one wake may run.
const MAX_FOLLOW_UPS: u32 = 8;
const DEFAULT_FOLLOW_UPS: u32 = 3;

/// Where the mind keeps its files. Everything lives under one home owned by
/// the service user, apart from the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MindPaths {
    pub config: PathBuf,
    pub home: PathBuf,
    pub socket: PathBuf,
    pub session_dir: PathBuf,
    pub state: PathBuf,
    pub log: PathBuf,
    pub lock: PathBuf,
}

impl MindPaths {
    /// Default paths under `home`, with `config` as the config file.
    #[must_use]
    pub fn under(config: PathBuf, home: &Path) -> Self {
        Self {
            config,
            home: home.to_path_buf(),
            socket: home.join("mind.sock"),
            session_dir: home.join("sessions"),
            state: home.join("state.json"),
            log: home.join("mind.log"),
            lock: home.join("mind.lock"),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFormat {
    authority: Option<AuthorityStage>,
    heartbeat: Option<String>,
    proxy_url: Option<String>,
    pi_command: Option<String>,
    model: Option<String>,
    human_only: Option<Vec<HumanOnlyCategory>>,
    socket: Option<PathBuf>,
    session_dir: Option<PathBuf>,
    #[serde(default)]
    budgets: BudgetsFormat,
    mcp: Option<McpFormat>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpFormat {
    server: Option<String>,
    key_file: Option<PathBuf>,
    follow_ups: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetsFormat {
    per_need_actions: Option<u32>,
    per_need_window: Option<String>,
    max_wakes_per_hour: Option<u32>,
}

/// Path of `mind.toml`, beside the host registry.
///
/// # Errors
/// Fails when no config directory can be found.
pub fn path() -> Result<PathBuf, VivariumError> {
    let registry = crate::registry::path()?;
    let dir = registry
        .parent()
        .ok_or_else(|| VivariumError::Config("registry path has no parent".into()))?;
    Ok(dir.join(CONFIG_FILENAME))
}

/// The default mind home: `VIVI_MIND_HOME`, else `<state_dir>/vivi/mind`,
/// else `<data_local_dir>/vivi/mind`.
///
/// # Errors
/// Fails when no state or data directory can be found.
pub fn default_home() -> Result<PathBuf, VivariumError> {
    if let Ok(value) = std::env::var(ENV_MIND_HOME)
        && !value.is_empty()
    {
        return Ok(PathBuf::from(value));
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|dir| dir.join("vivi").join("mind"))
        .ok_or_else(|| VivariumError::Config("could not find a state directory".into()))
}

/// Load the config from its default location.
///
/// # Errors
/// Fails on an unreadable or invalid file.
pub fn load() -> Result<MindConfig, VivariumError> {
    load_at(&path()?, &default_home()?)
}

/// Load the config at `path`, resolving default paths under `home`. A
/// missing file is the default config.
///
/// # Errors
/// Fails on an unreadable or invalid file.
pub fn load_at(path: &Path, home: &Path) -> Result<MindConfig, VivariumError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.into()),
    };
    parse(&text, MindPaths::under(path.to_path_buf(), home))
}

/// Parse `mind.toml` text over default `paths`.
///
/// # Errors
/// Fails on invalid TOML, an unknown key, a bad duration, a non-loopback
/// proxy, a malformed or duplicated `human_only` token, or an authority
/// stage this build does not implement.
pub fn parse(text: &str, mut paths: MindPaths) -> Result<MindConfig, VivariumError> {
    let file: FileFormat = toml::from_str(text)
        .map_err(|error| VivariumError::Config(format!("invalid mind.toml: {error}")))?;
    let authority = file.authority.unwrap_or(AuthorityStage::Observe);
    if !authority.implemented() {
        return Err(VivariumError::Config(format!(
            "authority stage {} is not implemented in this build",
            authority.as_str()
        )));
    }
    let heartbeat = file
        .heartbeat
        .unwrap_or_else(|| DEFAULT_HEARTBEAT.to_string());
    let heartbeat_secs = parse_duration_secs(&heartbeat)?;
    let proxy_url = file
        .proxy_url
        .unwrap_or_else(|| DEFAULT_PROXY_URL.to_string());
    require_loopback(&proxy_url)?;
    if let Some(socket) = file.socket {
        paths.socket = socket;
    }
    if let Some(session_dir) = file.session_dir {
        paths.session_dir = session_dir;
    }
    Ok(MindConfig {
        authority,
        heartbeat,
        heartbeat_secs,
        proxy_url,
        pi_command: file
            .pi_command
            .unwrap_or_else(|| DEFAULT_PI_COMMAND.to_string()),
        model: file.model,
        budgets: budgets(file.budgets)?,
        human_only: human_only_policy(file.human_only)?,
        mcp: mcp(file.mcp, &paths)?,
        paths,
    })
}

fn mcp(file: Option<McpFormat>, paths: &MindPaths) -> Result<Option<McpConfig>, VivariumError> {
    let Some(file) = file else {
        return Ok(None);
    };
    let server = file
        .server
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| VivariumError::Config("[mcp] needs a server URL".into()))?;
    if !(server.starts_with("http://") || server.starts_with("https://")) {
        return Err(VivariumError::Config(format!(
            "[mcp] server must be an http(s) URL, got {server:?}"
        )));
    }
    let follow_ups = file.follow_ups.unwrap_or(DEFAULT_FOLLOW_UPS);
    if follow_ups > MAX_FOLLOW_UPS {
        return Err(VivariumError::Config(format!(
            "[mcp] follow_ups is at most {MAX_FOLLOW_UPS}, got {follow_ups}"
        )));
    }
    Ok(Some(McpConfig {
        server,
        key_file: file.key_file.unwrap_or_else(|| paths.home.join("mcp.key")),
        follow_ups,
    }))
}

fn budgets(file: BudgetsFormat) -> Result<Budgets, VivariumError> {
    let defaults = Budgets::default();
    let per_need_window_secs = match file.per_need_window {
        Some(window) => parse_duration_secs(&window)?,
        None => defaults.per_need_window_secs,
    };
    Ok(Budgets {
        per_need_actions: file.per_need_actions.unwrap_or(defaults.per_need_actions),
        per_need_window_secs,
        max_wakes_per_hour: file
            .max_wakes_per_hour
            .unwrap_or(defaults.max_wakes_per_hour),
    })
}

/// Refuse a proxy URL whose host is not a loopback address or `localhost`.
///
/// # Errors
/// Fails on an unparseable URL or a non-loopback host.
pub fn require_loopback(url: &str) -> Result<(), VivariumError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| VivariumError::Config(format!("invalid proxy_url {url:?}: {error}")))?;
    let host = parsed
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(crate::proxy::config::is_loopback);
    if loopback {
        Ok(())
    } else {
        Err(VivariumError::Config(format!(
            "proxy_url {url:?} must be loopback: the mind reaches models only through the hub proxy"
        )))
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
