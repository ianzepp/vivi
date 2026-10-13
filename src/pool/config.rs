//! The `[pools.<name>]` tables in `.vivi/mailspace.toml`.
//!
//! A mailspace holds any number of named pools. Each table is complete on its
//! own: nothing is inherited from another pool and there are no shared
//! defaults. The pool's name is the table key.
//!
//! Parsing never fails a mailspace on a bad pool table beyond TOML shape;
//! [`PoolConfig::validate`] runs when a `vivi pool` command starts, and also
//! reports misspelled keys, which parsing keeps in `unknown`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::config_bands::PoolBands;
use super::runpod_config::PoolRunpod;
use super::slot::{DOCTOR_SUFFIX, PoolId};
use crate::error::VivariumError;

/// Machine hosts the pool can run on.
pub const PROVIDERS: [&str; 5] = ["fly", "docker", "ssh", "local", "runpod"];
const DEFAULT_PROVIDER: &str = "fly";
const DEFAULT_REGION: &str = "iad";
const DEFAULT_ORGANIZATION: &str = "personal";
const DEFAULT_SIZE: &str = "performance-8x";
const DEFAULT_MEMORY_MB: u32 = 32768;
const DEFAULT_TMPFS: &str = "16g";
const DEFAULT_DOCKER_MEMORY: &str = "4g";
const DEFAULT_DOCKER_TMPFS: &str = "2g";
/// Where an `ssh` pool keeps its slot directories when `root` is not set.
pub const DEFAULT_SSH_ROOT: &str = "~/vivi-pool";
/// Where a `local` pool keeps its slot directories when `root` is not set.
pub const DEFAULT_LOCAL_ROOT: &str = "~/vivi-pool";

/// Remote lane pool settings: one `[pools.<name>]` table. Machine-host
/// settings live in [`PoolFly`], [`PoolDocker`], [`PoolSsh`] and [`PoolLocal`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolConfig {
    /// Machine host: `fly`, `docker`, `ssh`, `local` or `runpod`.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Source repository, `owner/name`. Optional: a pool can be defined with
    /// only its provider settings (for testing). `create`, `scale` (when it
    /// creates) and `dispatch` need it, together with `github`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Maximum pool size. On `runpod` there are no resting slots: this is the
    /// most pods that may exist at once.
    pub slots: u32,
    /// Container image the machines run. Required by `fly`, `docker` and `runpod`;
    /// the `ssh` and `local` providers have no image, so it must be left out
    /// there.
    #[serde(default)]
    pub image: String,
    /// Shell commands run once at seed time, before the checkout: as root on
    /// `fly` and `docker`, as the login user on `ssh` and `local`.
    #[serde(default)]
    pub setup: Vec<String>,
    /// Shell commands run in the checkout at seed time to warm the build output.
    #[serde(default)]
    pub seed: Vec<String>,
    /// Shell commands that must each exit 0 on a slot for it to be usable.
    /// They run during seeding, in the checkout, after `setup` and before
    /// `seed`, as the user the worker will run as. One that fails fails the
    /// seed, and the slot stays `unseeded`.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Shell commands whose output is recorded in every job's result: run in
    /// the checkout as the worker's user before the harness starts, each under
    /// a short time limit and an output cap. A fact that fails or times out is
    /// recorded as such and does not fail the job.
    #[serde(default)]
    pub facts: Vec<String>,
    /// Build output directory relative to the repo; gets the memory disk and
    /// the pack (a plain, kept directory on `ssh` and `local`). No default:
    /// a pool that sets none has no build directory handling at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_dir: Option<String>,
    /// Git fetch depth for the seed fetch at create and every job's fetch at
    /// dispatch. Optional: the default is [`DEFAULT_FETCH_DEPTH`] recent
    /// commits, never the full history. No effect on a worktree pool, which
    /// fetches nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Repository-relative file paths put back to the start commit before a
    /// job commits: a build that rewrites a tracked file (a lock file, say)
    /// must not publish the rewrite. Default empty.
    #[serde(default)]
    pub restore: Vec<String>,
    /// The intelligence bands this pool serves. Optional: a pool with no
    /// `bands` table serves nothing by band and dispatches only by an
    /// explicit model. See [`super::config_bands`] and [`super::bands`].
    #[serde(default, skip_serializing_if = "PoolBands::is_empty")]
    pub bands: PoolBands,
    /// Settings that belong to the `fly` machine host.
    #[serde(default)]
    pub fly: PoolFly,
    /// Settings that belong to the `docker` machine host.
    #[serde(default)]
    pub docker: PoolDocker,
    /// Settings that belong to the `ssh` machine host.
    #[serde(default)]
    pub ssh: PoolSsh,
    /// Settings that belong to the `local` machine host.
    #[serde(default)]
    pub local: PoolLocal,
    /// Settings that belong to the `runpod` machine host.
    #[serde(default)]
    pub runpod: PoolRunpod,
    /// GitHub App used to mint the fetch token. Optional, and required
    /// together with `repo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<PoolGithub>,
    /// Agent harness description; `dispatch` needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<PoolHarness>,
    /// Identity the remote runner commits as; `dispatch` needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committer: Option<PoolCommitter>,
    /// Source delivered with each slot instead of fetched from `repo`. Never
    /// read from TOML: only the live self-test of `vivi pool doctor --test`
    /// sets it.
    #[serde(skip)]
    pub embedded: Option<EmbeddedSource>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// A small source tree that travels with a slot and becomes a bare Git
/// remote on the slot's own disk, so jobs fetch and push locally.
#[derive(Debug, Clone, Default)]
pub struct EmbeddedSource {
    /// The `file://` address of the bare remote inside the slot.
    pub remote: String,
    /// `(guest path, contents)` of the source files.
    pub files: Vec<(String, String)>,
    /// Extra machine environment (for example boot timing).
    pub env: BTreeMap<String, String>,
}

/// The `[pools.<name>.fly]` table: the settings only the `fly` machine host
/// has.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolFly {
    /// Provider application that holds the machines. Required for a `fly`
    /// pool and without a default: Fly app names are unique across all Fly
    /// accounts, and a pool name is a short word that would collide. `create`
    /// makes the application when it does not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// Provider organization that owns a newly created application.
    /// Default: `personal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default = "default_region")]
    pub region: String,
    /// Provider machine size (CPU class).
    #[serde(default = "default_size")]
    pub size: String,
    /// Machine memory in MB.
    #[serde(default = "default_memory")]
    pub memory: u32,
    /// Size of the memory disk mounted over the build output, e.g. `16g`.
    #[serde(default = "default_tmpfs")]
    pub tmpfs: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for PoolFly {
    fn default() -> Self {
        Self {
            app: None,
            organization: None,
            region: default_region(),
            size: default_size(),
            memory: default_memory(),
            tmpfs: default_tmpfs(),
            unknown: BTreeMap::new(),
        }
    }
}

/// The `[pools.<name>.docker]` table: the settings only the `docker` machine host has.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolDocker {
    /// Docker daemon address, e.g. `ssh://user@server`. Omit for the local
    /// daemon (or whatever `DOCKER_HOST` names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Memory limit of each slot, e.g. `4g`. The memory disk over the build
    /// output counts against it.
    #[serde(default = "default_docker_memory")]
    pub memory: String,
    /// CPU limit of each slot (fractions allowed). Default: no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<f64>,
    /// Size of the memory disk mounted over the build output, e.g. `2g`.
    #[serde(default = "default_docker_tmpfs")]
    pub tmpfs: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl Default for PoolDocker {
    fn default() -> Self {
        Self {
            host: None,
            memory: default_docker_memory(),
            cpus: None,
            tmpfs: default_docker_tmpfs(),
            unknown: BTreeMap::new(),
        }
    }
}

/// The `[pools.<name>.ssh]` table: the settings only the `ssh` machine host has. A
/// slot is a directory on that host and the agent runs as the login user.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PoolSsh {
    /// What `ssh` is given to reach the host: `user@server`, or a `Host`
    /// alias from `~/.ssh/config`. Required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Directory on the host that holds the pools:
    /// `<root>/<mailspace>/<pool>/slot-<n>/`. `~/` means the login user's
    /// home. Default: `~/vivi-pool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl PoolSsh {
    /// The slot root as configured, or the default.
    #[must_use]
    pub fn root_or_default(&self) -> &str {
        self.root.as_deref().unwrap_or(DEFAULT_SSH_ROOT)
    }
}

/// The `[pools.<name>.local]` table: the settings only the `local` machine
/// host has. A slot is a directory on this machine and the worker runs as the
/// current user, with no isolation.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PoolLocal {
    /// Directory that holds the pools:
    /// `<root>/<mailspace>/<pool>/slot-<n>/`. `~/` means the home
    /// directory. Default: `~/vivi-pool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// A Git repository already on this machine. When set, each slot's
    /// checkout is a Git worktree of it (no `repo`, no `github`, no token, no
    /// network) and a job's result is a commit on its branch in that
    /// repository. When not set, the slot fetches from a remote like the
    /// `ssh` provider does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_of: Option<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

impl PoolLocal {
    /// The slot root as configured, or the default.
    #[must_use]
    pub fn root_or_default(&self) -> &str {
        self.root.as_deref().unwrap_or(DEFAULT_LOCAL_ROOT)
    }

    /// The repository the slots are worktrees of, with a leading `~/`
    /// expanded.
    #[must_use]
    pub fn worktree_path(&self) -> Option<PathBuf> {
        let raw = self.worktree_of.as_deref()?;
        Some(match raw.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(raw), |h| h.join(rest)),
            None => PathBuf::from(raw),
        })
    }
}

impl PoolFly {
    /// The organization a new application is created in.
    #[must_use]
    pub fn organization_name(&self) -> &str {
        self.organization.as_deref().unwrap_or(DEFAULT_ORGANIZATION)
    }
}

/// A pool's repository with the GitHub App that reads it: both are set, or
/// neither is.
#[derive(Debug, Clone, Copy)]
pub struct RepoAccess<'a> {
    pub repo: &'a str,
    pub github: &'a PoolGithub,
}

/// GitHub App credentials for the fetch token.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolGithub {
    pub app_id: u64,
    /// Path to the App's PEM private key. `~` expands.
    pub private_key: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// The `[pools.<name>.committer]` table: the Git identity of remote commits.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolCommitter {
    pub name: String,
    pub email: String,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

/// The `[pools.<name>.harness]` table.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PoolHarness {
    pub name: String,
    /// Command template that runs the harness headlessly. Placeholders:
    /// `{model}`, `{brief}` (path of the task body), `{rules}` (path of the
    /// standing rules). Its standard output is the receipt.
    pub run: String,
    /// Model slugs the harness may be given.
    #[serde(default)]
    pub models: Vec<String>,
    /// Names of environment variables the harness receives, read from the
    /// machine's own environment. Names only; never values.
    #[serde(default)]
    pub env: Vec<String>,
    /// Standing rules file for the remote worker. `~` expands; a relative
    /// path is relative to the project root. Default: the embedded rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, toml::Value>,
}

fn default_provider() -> String {
    DEFAULT_PROVIDER.to_string()
}
fn default_region() -> String {
    DEFAULT_REGION.to_string()
}
fn default_size() -> String {
    DEFAULT_SIZE.to_string()
}
fn default_memory() -> u32 {
    DEFAULT_MEMORY_MB
}
fn default_tmpfs() -> String {
    DEFAULT_TMPFS.to_string()
}
fn default_docker_memory() -> String {
    DEFAULT_DOCKER_MEMORY.to_string()
}
fn default_docker_tmpfs() -> String {
    DEFAULT_DOCKER_TMPFS.to_string()
}

impl PoolConfig {
    /// Size of the memory disk over the build output, from the active host.
    #[must_use]
    pub fn tmpfs_size(&self) -> &str {
        match self.provider.as_str() {
            "docker" => &self.docker.tmpfs,
            // A directory on a host has no memory disk.
            "ssh" | "local" | "runpod" => "",
            _ => &self.fly.tmpfs,
        }
    }

    /// The local repository the slots are worktrees of, for a `local` pool in
    /// worktree mode.
    #[must_use]
    pub fn worktree_source(&self) -> Option<PathBuf> {
        if self.provider == "local" {
            self.local.worktree_path()
        } else {
            None
        }
    }

    /// The full Git address jobs and the seed fetch from and push to:
    /// GitHub for `repo = "owner/name"`, or the embedded source's own remote.
    /// Empty for a pool with neither (commands that need one check first with
    /// [`PoolConfig::require_repo`]) and for a worktree pool, which has no
    /// remote.
    #[must_use]
    pub fn remote_address(&self) -> String {
        match (&self.embedded, &self.repo) {
            (Some(source), _) => source.remote.clone(),
            (None, Some(repo)) => format!("https://github.com/{repo}.git"),
            (None, None) => String::new(),
        }
    }

    /// The repository and its GitHub App, when the pool has them.
    #[must_use]
    pub fn repo_access(&self) -> Option<RepoAccess<'_>> {
        Some(RepoAccess {
            repo: self.repo.as_deref()?,
            github: self.github.as_ref()?,
        })
    }

    /// The repository and its GitHub App for a command that needs them
    /// (`what` says which command).
    ///
    /// # Errors
    /// Returns a configuration error naming the missing setting.
    pub fn require_repo(&self, pool: &str, what: &str) -> Result<RepoAccess<'_>, VivariumError> {
        let missing = |setting: &str| {
            VivariumError::Config(format!(
                "[pools.{pool}] {setting} is required to {what} (a pool defined with only its provider settings can be checked, listed, waited on, reconciled and destroyed, but not created or dispatched to)"
            ))
        };
        let repo = self.repo.as_deref().ok_or_else(|| missing("repo"))?;
        let github = self.github.as_ref().ok_or_else(|| missing("github"))?;
        Ok(RepoAccess { repo, github })
    }

    /// The organization a new application is created in.
    #[must_use]
    pub fn organization_name(&self) -> &str {
        self.fly.organization_name()
    }

    /// Check the table before any command acts on it. `id` is the pool's
    /// identity: its name is the table key, and the mailspace slug is part of
    /// every mark and machine name. Every message names the pool.
    ///
    /// # Errors
    /// Returns a configuration error naming the first bad field.
    pub fn validate(&self, id: &PoolId) -> Result<(), VivariumError> {
        self.validate_fields(id).map_err(|e| match e {
            VivariumError::Config(message) => {
                VivariumError::Config(format!("[pools.{}] {message}", id.pool))
            }
            other => other,
        })?;
        self.check_depth(&id.pool)?;
        self.check_restore(&id.pool)
    }

    fn validate_fields(&self, id: &PoolId) -> Result<(), VivariumError> {
        self.validate_identity(id)?;
        self.validate_machine(&id.pool)?;
        if self.provider == "runpod" {
            super::runpod_name::check_fits(id)?;
        }
        self.validate_commands()?;
        self.validate_github()?;
        let pool = id.pool.as_str();
        if let Some(harness) = &self.harness {
            harness.validate(pool)?;
        }
        if let Some(committer) = &self.committer {
            committer.validate(pool)?;
        }
        self.check_bands(pool)?;
        reject_unknown(pool, "", &self.unknown)?;
        reject_unknown(pool, ".fly", &self.fly.unknown)?;
        reject_unknown(pool, ".docker", &self.docker.unknown)?;
        reject_unknown(pool, ".ssh", &self.ssh.unknown)?;
        reject_unknown(pool, ".local", &self.local.unknown)?;
        reject_unknown(pool, ".runpod", &self.runpod.unknown)?;
        match &self.github {
            Some(github) => reject_unknown(pool, ".github", &github.unknown),
            None => Ok(()),
        }
    }

    fn validate_identity(&self, id: &PoolId) -> Result<(), VivariumError> {
        if !PROVIDERS.contains(&self.provider.as_str()) {
            return Err(bad(format!(
                "provider \"{}\" is not supported (use \"fly\", \"docker\", \"ssh\", \"local\" or \"runpod\")",
                self.provider
            )));
        }
        check_pool_name(&id.pool)?;
        if id.mailspace.is_empty() {
            return Err(bad(
                "the mailspace name has no letter or digit, so it cannot name machines; rename the mailspace in .vivi/mailspace.toml",
            ));
        }
        if self.provider == "fly" {
            self.validate_fly_names()?;
        }
        match (&self.repo, &self.github) {
            (Some(_), None) => return Err(bad("github is required when repo is set")),
            (None, Some(_)) => return Err(bad("repo is required when github is set")),
            _ => {}
        }
        if self.repo.is_some() && self.local.worktree_of.is_some() {
            return Err(bad(
                "worktree_of and repo cannot both be set (a worktree pool takes its source from a repository on this machine, not from GitHub)",
            ));
        }
        let Some(repo) = &self.repo else {
            return Ok(());
        };
        let mut parts = repo.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(name), None) if is_repo_part(owner) && is_repo_part(name) => Ok(()),
            _ => Err(bad("repo must be owner/name")),
        }
    }

    fn validate_fly_names(&self) -> Result<(), VivariumError> {
        let Some(app) = &self.fly.app else {
            return Err(bad(
                "fly.app is required for a fly pool and has no default (Fly app names are unique across all Fly accounts); set `app = \"<your-unique-app-name>\"` in [fly]",
            ));
        };
        let org = self.organization_name().to_string();
        for (field, value) in [
            ("app", app),
            ("organization", &org),
            ("region", &self.fly.region),
        ] {
            if !is_slug(value) {
                return Err(bad(format!(
                    "{field} must be lowercase letters, digits, and dashes"
                )));
            }
        }
        Ok(())
    }

    fn validate_machine(&self, pool: &str) -> Result<(), VivariumError> {
        if self.slots == 0 {
            return Err(bad("slots must be at least 1"));
        }
        self.validate_image()?;
        match self.provider.as_str() {
            "docker" => self.docker.validate()?,
            "ssh" => self.ssh.validate()?,
            "local" => self.local.validate()?,
            "runpod" => self.runpod.validate(pool)?,
            _ => self.validate_fly_machine()?,
        }
        self.build_dir.as_deref().map_or(Ok(()), validate_build_dir)
    }

    fn validate_image(&self) -> Result<(), VivariumError> {
        if matches!(self.provider.as_str(), "ssh" | "local") {
            return if self.image.is_empty() {
                Ok(())
            } else {
                Err(bad(format!(
                    "image does not apply to the {} provider (a slot is a directory on the host, not a container); remove it",
                    self.provider
                )))
            };
        }
        if self.image.trim().is_empty() || self.image.chars().any(char::is_whitespace) {
            return Err(bad("image must be a single word"));
        }
        Ok(())
    }

    fn validate_fly_machine(&self) -> Result<(), VivariumError> {
        if self.fly.memory == 0 {
            return Err(bad("memory must be positive (MB)"));
        }
        let size = &self.fly.size;
        if size.trim().is_empty() || size.chars().any(char::is_whitespace) {
            return Err(bad("size must be a single word"));
        }
        if !is_tmpfs_size(&self.fly.tmpfs) {
            return Err(bad("tmpfs must look like 16g, 512m, or 4096k"));
        }
        Ok(())
    }

    fn validate_commands(&self) -> Result<(), VivariumError> {
        for (field, list) in [("setup", &self.setup), ("seed", &self.seed)] {
            if list.iter().any(|c| c.trim().is_empty()) {
                return Err(bad(format!("{field} holds an empty command")));
            }
        }
        for (field, list) in [("requires", &self.requires), ("facts", &self.facts)] {
            if list.iter().any(|c| c.trim().is_empty()) {
                return Err(bad(format!("{field} holds an empty command")));
            }
            if list
                .iter()
                .any(|c| c.chars().any(|ch| ch.is_control() && ch != '\t'))
            {
                return Err(bad(format!(
                    "{field} holds a command with a line break or control character (one command per entry, on one line)"
                )));
            }
        }
        Ok(())
    }

    fn validate_github(&self) -> Result<(), VivariumError> {
        let Some(github) = &self.github else {
            return Ok(());
        };
        if github.app_id == 0 {
            return Err(bad("github.app_id must be nonzero"));
        }
        if github.private_key.trim().is_empty() {
            return Err(bad("github.private_key must name a key file"));
        }
        Ok(())
    }
}

impl PoolDocker {
    fn validate(&self) -> Result<(), VivariumError> {
        if let Some(host) = &self.host
            && !is_docker_host(host)
        {
            return Err(bad(
                "docker.host must look like ssh://user@server (or tcp://, unix://)",
            ));
        }
        if !is_tmpfs_size(&self.memory) {
            return Err(bad("docker.memory must look like 4g, 512m, or 4096k"));
        }
        if !is_tmpfs_size(&self.tmpfs) {
            return Err(bad("docker.tmpfs must look like 2g, 512m, or 4096k"));
        }
        if self.cpus.is_some_and(|c| !(c.is_finite() && c > 0.0)) {
            return Err(bad("docker.cpus must be a positive number"));
        }
        Ok(())
    }
}

impl PoolSsh {
    fn validate(&self) -> Result<(), VivariumError> {
        let Some(host) = &self.host else {
            return Err(bad(
                "ssh.host is required (user@server, or a Host alias from ~/.ssh/config)",
            ));
        };
        check_ssh_host(host).map_err(bad)?;
        check_ssh_root(self.root_or_default()).map_err(bad)
    }
}

impl PoolLocal {
    fn validate(&self) -> Result<(), VivariumError> {
        check_root(self.root_or_default(), "local.root").map_err(bad)?;
        match &self.worktree_of {
            Some(path) => check_worktree_of(path).map_err(bad),
            None => Ok(()),
        }
    }
}

/// Check the repository a worktree pool is made from: an absolute path or
/// `~/<path>`, with no line break, `..` part or empty part. It may hold any
/// other character a directory name can.
///
/// # Errors
/// Returns the reason the path cannot be used.
pub fn check_worktree_of(path: &str) -> Result<(), String> {
    let rest = path.strip_prefix("~/").or_else(|| path.strip_prefix('/'));
    let parts_ok = |rest: &str| {
        !rest.is_empty()
            && rest
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
    };
    match rest {
        Some(rest) if parts_ok(rest) && !path.chars().any(char::is_control) => Ok(()),
        _ => Err(
            "local.worktree_of must be the path of a Git repository on this machine: /abs/path or ~/path (no .. and no line break)"
                .to_string(),
        ),
    }
}

/// Check what `ssh` will be given as the host.
///
/// # Errors
/// Returns the reason the host cannot be used.
pub fn check_ssh_host(host: &str) -> Result<(), String> {
    let plain = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '.' | '_' | '@' | ':' | '-' | '[' | ']' | '%' | '+')
    };
    if host.is_empty() || host.starts_with('-') || !host.chars().all(plain) {
        return Err(
            "ssh.host must be user@server or a Host alias (letters, digits and . _ @ : - only, not starting with a dash)"
                .to_string(),
        );
    }
    Ok(())
}

/// Check the directory that holds an `ssh` pool's slots. See [`check_root`].
///
/// # Errors
/// Returns the reason the root cannot be used.
pub fn check_ssh_root(root: &str) -> Result<(), String> {
    check_root(root, "ssh.root")
}

/// Check the directory that holds a directory pool's slots (`key` names the
/// setting in the message). It is deleted from, so it must be an explicit
/// directory of its own: `~/<path>` or an absolute path, plain characters
/// only, never the home directory or `/` itself.
///
/// # Errors
/// Returns the reason the root cannot be used.
pub fn check_root(root: &str, key: &str) -> Result<(), String> {
    let rest = root.strip_prefix("~/").or_else(|| root.strip_prefix('/'));
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/');
    let parts_ok = |rest: &str| {
        !rest.is_empty()
            && rest
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
    };
    match rest {
        Some(rest) if root.chars().skip(1).all(plain) && parts_ok(rest) => Ok(()),
        _ => Err(format!(
            "{key} must be a directory of its own: ~/some/dir or /abs/dir (letters, digits and . _ - / only; no .., not / or the home directory itself)"
        )),
    }
}

impl PoolGithub {
    /// The private key path with a leading `~` expanded.
    #[must_use]
    pub fn private_key_path(&self) -> PathBuf {
        match self.private_key.strip_prefix("~/") {
            Some(rest) => dirs::home_dir()
                .map_or_else(|| PathBuf::from(&self.private_key), |home| home.join(rest)),
            None => PathBuf::from(&self.private_key),
        }
    }
}

impl PoolCommitter {
    fn validate(&self, pool: &str) -> Result<(), VivariumError> {
        for (field, value) in [("name", &self.name), ("email", &self.email)] {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                return Err(bad(format!("committer.{field} must be a single line")));
            }
        }
        reject_unknown(pool, ".committer", &self.unknown)
    }
}

impl PoolHarness {
    /// The rules file path with `~` expanded and a relative path anchored at
    /// the project root; `None` means the embedded default.
    #[must_use]
    pub fn rules_path(&self, root: &std::path::Path) -> Option<PathBuf> {
        let raw = self.rules.as_deref()?;
        Some(match raw.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().map_or_else(|| PathBuf::from(raw), |h| h.join(rest)),
            None => root.join(raw),
        })
    }

    fn validate(&self, pool: &str) -> Result<(), VivariumError> {
        if self.name.trim().is_empty() {
            return Err(bad("harness.name must not be empty"));
        }
        if self.run.trim().is_empty() {
            return Err(bad("harness.run must not be empty"));
        }
        if let Some(unknown) = super::job::unknown_placeholder(&self.run) {
            return Err(bad(format!(
                "harness.run holds the unknown placeholder {{{unknown}}} (use {{model}}, {{brief}}, {{rules}})"
            )));
        }
        if self.models.iter().any(|m| !is_model_slug(m)) {
            return Err(bad(
                "harness.models holds an empty slug or one with characters outside A-Z a-z 0-9 . _ : / @ + -",
            ));
        }
        for name in &self.env {
            if !is_env_name(name) || name == "GH_TOKEN" {
                return Err(bad(format!(
                    "harness.env holds \"{name}\": use plain variable names, never GH_TOKEN"
                )));
            }
        }
        let mut seen = self.models.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.models.len() {
            return Err(bad("harness.models holds a duplicate slug"));
        }
        reject_unknown(pool, ".harness", &self.unknown)
    }
}

fn validate_build_dir(dir: &str) -> Result<(), VivariumError> {
    let safe_chars = dir
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'));
    let escapes = dir.split('/').any(|part| part == ".." || part == ".");
    if dir.is_empty() || dir.starts_with('/') || !safe_chars || escapes || dir.contains("//") {
        return Err(bad(
            "build_dir must be a plain relative path inside the repo, e.g. target",
        ));
    }
    Ok(())
}

fn is_model_slug(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '@' | '+' | '-')
        })
}

fn is_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_docker_host(value: &str) -> bool {
    let schemes = ["ssh://", "tcp://", "unix://"];
    schemes
        .iter()
        .any(|s| value.strip_prefix(s).is_some_and(|rest| !rest.is_empty()))
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn is_tmpfs_size(value: &str) -> bool {
    let Some(last) = value.chars().last() else {
        return false;
    };
    let digits = &value[..value.len() - last.len_utf8()];
    matches!(last, 'k' | 'K' | 'm' | 'M' | 'g' | 'G')
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && digits.parse::<u64>().is_ok_and(|n| n > 0)
}

/// Check a pool's name: the key of its `[pools.<name>]` table. It becomes part
/// of machine names, marks and directories, so it is a slug that starts and
/// ends with a letter or digit, and it never ends with the suffix that names
/// the pool's self-test pool.
///
/// # Errors
/// Returns a configuration error saying what the name must look like.
pub fn check_pool_name(name: &str) -> Result<(), VivariumError> {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !is_slug(name) || !edge(name.chars().next()) || !edge(name.chars().last()) {
        return Err(bad(
            "the pool name (the key of its table) must be lowercase letters, digits and dashes, starting and ending with a letter or digit",
        ));
    }
    if name.ends_with(DOCTOR_SUFFIX) {
        return Err(bad(format!(
            "the pool name must not end with \"{DOCTOR_SUFFIX}\" (that names the self-test pool of another pool)"
        )));
    }
    Ok(())
}

pub(super) fn reject_unknown(
    pool: &str,
    table: &str,
    unknown: &BTreeMap<String, toml::Value>,
) -> Result<(), VivariumError> {
    match unknown.keys().next() {
        Some(key) => Err(bad(format!(
            "unknown field \"{key}\" in [pools.{pool}{table}]"
        ))),
        None => Ok(()),
    }
}

pub(super) fn bad(message: impl Into<String>) -> VivariumError {
    VivariumError::Config(message.into())
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;

#[cfg(test)]
#[path = "config_local_test.rs"]
mod local_tests;
