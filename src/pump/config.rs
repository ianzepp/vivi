//! Pump configuration (the `[pump]` table of `mailspace.toml`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use self::config_workspace::PumpWorkspace;

mod budgets;
mod check;
#[path = "config_workspace.rs"]
pub mod config_workspace;
mod flow;
mod migrate;
mod origins;
mod planner;
mod rungs;
mod show;
pub mod targets;
mod validate;

pub use budgets::StepBudgets;
pub use check::{ValidateReport, issues_json, validate};
pub use flow::{DEFAULT_SLICE_MINUTES, FlowConfig, SliceConfig};
pub use migrate::{MigrateReport, WrittenCharter, migrate};
pub use planner::PlannerConfig;
pub use rungs::{DEFAULT_RUNG_TIMEOUT, RUNG_NAMES, RungsConfig};
pub use show::{ShowReport, show};
pub use validate::{Findings, Missing, valid_target_id};

/// Pump settings (the `[pump]` table in `mailspace.toml`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PumpConfig {
    /// Pump home directory; relative paths join the project root and `~` is
    /// not expanded. Default `<project>/.vivi/pump`.
    pub home: Option<String>,
    /// The integration clone; cloned from `remote_url` at startup when
    /// absent. Default `<home>/repo`.
    pub repo: Option<String>,
    /// Git remote (name) main lives on.
    pub remote: String,
    /// URL or path the integration clone is created from when `repo` is
    /// missing at startup; the clone's remote is named `remote`. With none
    /// set, a missing `repo` stops the pump with an error.
    pub remote_url: Option<String>,
    /// Named repository execution targets within this mailspace. When empty,
    /// the legacy single-repository settings form the `default` target.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub targets: BTreeMap<String, PumpTarget>,
    /// Git author and committer name of every commit and merge the pump
    /// makes. Required: `validate` rejects an empty one.
    pub committer_name: String,
    /// Git author and committer email of every commit and merge the pump
    /// makes. Required: `validate` rejects an empty one.
    pub committer_email: String,
    /// The main branch units start from and merge into.
    pub main: String,
    /// Seconds between ticks.
    pub tick: u64,
    /// Shell fragment prepended to every command line.
    pub shell_prefix: String,
    /// File whose text opens every unit brief; absent means the embedded
    /// default text. A relative path joins the project root. `validate`
    /// rejects a path that cannot be read.
    pub hand_rules: Option<String>,
    /// Paths held by another session; units overlapping them are not
    /// dispatched. A relative value resolves against `home`. Default
    /// `reserved.txt`.
    pub reserved_file: String,
    /// Retired: need priority lives in `pump.db` (`vivi pump priority`). The
    /// key is accepted and ignored so an old `priority_file = "priority.txt"`
    /// line in `mailspace.toml` still parses; it is never written back.
    #[serde(rename = "priority_file", skip_serializing)]
    pub retired_priority_file: Option<String>,
    /// Unit ids with this prefix are fix units.
    pub fix_prefix: String,
    /// Role the pump sends unit tasks from.
    pub from: String,
    /// Recipient role of unit tasks.
    pub to: String,
    /// Role that replies on need threads.
    pub reply_from: String,
    /// Board pass settings (the `[pump.board]` table).
    pub board: BoardConfig,
    /// Planner settings (the `[pump.planner]` table).
    pub planner: PlannerConfig,
    /// Proof-validation settings (the `[pump.proof]` table).
    pub proof: ProofConfig,
    /// Plan-check settings (the `[pump.plan]` table).
    #[serde(skip_serializing_if = "PlanConfig::is_empty")]
    pub plan: PlanConfig,
    /// Dispatch settings (the `[pump.dispatch]` table).
    pub dispatch: DispatchConfig,
    /// Merge settings (the `[pump.merge]` table).
    pub merge: MergeConfig,
    /// Lane RAM disk settings (the `[pump.workspace]` table).
    pub workspace: PumpWorkspace,
    /// Rung settings (the `[pump.rungs]` table).
    #[serde(skip_serializing_if = "RungsConfig::is_empty")]
    pub rungs: RungsConfig,
    /// Flow settings (the `[pump.flow]` table).
    #[serde(skip_serializing_if = "FlowConfig::is_empty")]
    pub flow: FlowConfig,
    /// Slice settings (the `[pump.slice]` table).
    #[serde(skip_serializing_if = "SliceConfig::is_empty")]
    pub slice: SliceConfig,
}

impl Default for PumpConfig {
    fn default() -> Self {
        Self {
            home: None,
            repo: None,
            remote: "origin".into(),
            remote_url: None,
            targets: BTreeMap::new(),
            committer_name: String::new(),
            committer_email: String::new(),
            main: "main".into(),
            tick: 20,
            shell_prefix: String::new(),
            hand_rules: None,
            reserved_file: "reserved.txt".into(),
            retired_priority_file: None,
            fix_prefix: "pumpfix".into(),
            from: "operator".into(),
            to: "hand".into(),
            reply_from: "mind".into(),
            board: BoardConfig::default(),
            planner: PlannerConfig::default(),
            proof: ProofConfig::default(),
            plan: PlanConfig::default(),
            dispatch: DispatchConfig::default(),
            merge: MergeConfig::default(),
            workspace: PumpWorkspace::default(),
            rungs: RungsConfig::default(),
            flow: FlowConfig::default(),
            slice: SliceConfig::default(),
        }
    }
}

impl PumpConfig {
    /// Effective repository targets. Legacy single-repository fields become
    /// one `default` target; explicit targets inherit only shared merge
    /// defaults.
    #[must_use]
    pub fn target_configs(&self) -> BTreeMap<String, PumpTarget> {
        if self.targets.is_empty() {
            return BTreeMap::from([(
                String::from("default"),
                PumpTarget {
                    path: String::from("."),
                    remote_url: self.remote_url.clone(),
                    remote: self.remote.clone(),
                    main: self.main.clone(),
                    integration_repo: self.repo.clone(),
                    merge: Some(self.merge.clone()),
                    ..PumpTarget::default()
                },
            )]);
        }
        self.targets
            .iter()
            .map(|(id, target)| {
                let mut target = target.clone();
                target.merge.get_or_insert_with(|| self.merge.clone());
                (id.clone(), target)
            })
            .collect()
    }

    /// The target a job without one works on: `default` when it is an
    /// explicit target, else the only explicit target. None without explicit
    /// targets (legacy fields keep the pool's own repository) or with several
    /// and no `default`.
    #[must_use]
    pub fn primary_target(&self) -> Option<String> {
        if self.targets.contains_key("default") {
            return Some(String::from("default"));
        }
        match self.targets.keys().collect::<Vec<_>>().as_slice() {
            [only] => Some((*only).clone()),
            _ => None,
        }
    }

    /// Generated paths declared for a named target: the workspace-level
    /// `[pump.merge]` table when no targets are configured, else the named
    /// target's effective merge table. Unknown targets have none.
    #[must_use]
    pub fn generated_paths_for(&self, target: &str) -> Vec<String> {
        if self.targets.is_empty() {
            return self.merge.generated_paths();
        }
        self.target_configs()
            .get(target)
            .and_then(|target| target.merge.as_ref())
            .map_or_else(Vec::new, MergeConfig::generated_paths)
    }

    /// The command of rung `name` for `target`: the target's own rung wins
    /// over the workspace-level `[pump.rungs]` one. None when neither sets it.
    #[must_use]
    pub fn rung(&self, target: &str, name: &str) -> Option<&str> {
        self.targets
            .get(target)
            .and_then(|target| target.rungs.command(name))
            .or_else(|| self.rungs.command(name))
    }

    /// The workspace-relative source repository path for a named target.
    #[must_use]
    pub fn target_path(&self, project: &Path, id: &str) -> Option<PathBuf> {
        self.target_configs()
            .get(id)
            .map(|target| resolve(project, &target.path))
    }

    /// Target-specific integration checkout. Legacy config keeps its old
    /// checkout path; named targets get an isolated checkout under pump home.
    #[must_use]
    pub fn target_repo_dir(&self, project: &Path, id: &str) -> Option<PathBuf> {
        let target = self.target_configs().remove(id)?;
        if self.targets.is_empty() {
            return Some(self.repo_dir(project));
        }
        let home = self.home_dir(project).join("targets").join(id);
        Some(
            target
                .integration_repo
                .as_deref()
                .map_or_else(|| home.join("repo"), |path| resolve(&home, path)),
        )
    }

    /// A target-specific copy of the legacy pump settings consumed by the
    /// single-target execution seams.
    #[must_use]
    pub fn for_target(&self, id: &str) -> Option<Self> {
        let target = self.target_configs().remove(id)?;
        let mut config = self.clone();
        config.repo = target.integration_repo;
        config.remote_url = target.remote_url;
        config.remote = target.remote;
        config.main = target.main;
        config.merge = target.merge.unwrap_or_default();
        Some(config)
    }

    /// Pool name → the local repository its units commit into, for the `local`
    /// pools a single-target pump dispatches into.
    ///
    /// A pool is included exactly when [`crate::pool::target::resolve`] would
    /// treat it as local: its provider is `local`, and it either names a
    /// `worktree_of` repository or names no GitHub source. Its value is the
    /// worktree repository, else the project checkout the pool falls back to.
    ///
    /// Multi-target pumps get an empty map: each target reads delivered
    /// branches from its own integration clone, so a flat pool-to-path map
    /// would name the wrong repository.
    #[must_use]
    pub fn pool_sources(
        &self,
        config: &crate::mailspace::MailspaceConfig,
        project: &Path,
    ) -> BTreeMap<String, PathBuf> {
        if !self.targets.is_empty() {
            return BTreeMap::new();
        }
        config
            .pools
            .iter()
            .filter(|(_, pool)| {
                pool.provider == "local"
                    && (pool.local.worktree_of.is_some() || pool.github.is_none())
            })
            .map(|(name, pool)| {
                let source = pool
                    .worktree_source()
                    .unwrap_or_else(|| project.to_path_buf());
                (name.clone(), source)
            })
            .collect()
    }

    /// The pump home: `home` (relative paths join `project`) or
    /// `<project>/.vivi/pump`.
    #[must_use]
    pub fn home_dir(&self, project: &Path) -> PathBuf {
        match &self.home {
            Some(home) => resolve(project, home),
            None => project.join(".vivi").join("pump"),
        }
    }

    /// The integration clone: `repo` (relative paths join `project`) or
    /// `<home>/repo`.
    #[must_use]
    pub fn repo_dir(&self, project: &Path) -> PathBuf {
        match &self.repo {
            Some(repo) => resolve(project, repo),
            None => self.home_dir(project).join("repo"),
        }
    }

    /// The hand-rules file: `hand_rules` (relative paths join `project`), or
    /// `None` when unset.
    #[must_use]
    pub fn hand_rules_path(&self, project: &Path) -> Option<PathBuf> {
        self.hand_rules
            .as_deref()
            .map(|rules| resolve(project, rules))
    }

    /// The reserved-paths file: `reserved_file`, a relative value joining
    /// `home`.
    #[must_use]
    pub fn reserved_path(&self, project: &Path) -> PathBuf {
        resolve(&self.home_dir(project), &self.reserved_file)
    }

    /// `--from` of a unit dispatch: `dispatch.from`, else `main`.
    #[must_use]
    pub fn dispatch_from(&self) -> &str {
        self.dispatch.from.as_deref().unwrap_or(&self.main)
    }

    /// `--to` of the pump's own sends and thread replies: `board.reply_to`,
    /// else the first of `board.from`, else `operator`. It is never empty: an
    /// empty value would drop the argument of `need send --to`, and that send
    /// would fail on every tick.
    #[must_use]
    pub fn reply_to(&self) -> String {
        self.board
            .reply_to
            .clone()
            .or_else(|| self.board.from.first().cloned())
            .unwrap_or_else(|| String::from("operator"))
    }

    /// The kind a unit's `tools` value selects (`default_kind` when `tools` is
    /// empty); `None` when it is not defined.
    #[must_use]
    pub fn kind_of(&self, tools: &str) -> Option<&Kind> {
        let name = if tools.is_empty() {
            &self.dispatch.default_kind
        } else {
            tools
        };
        self.dispatch.kinds.get(name)
    }
}

/// Repository-specific integration settings. Pool capacity and provider
/// configuration remain shared by every target in the mailspace.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PumpTarget {
    /// Repository root relative to the workspace/mailspace root.
    pub path: String,
    /// GitHub owner/name used for target-scoped job credentials.
    pub github_repo: Option<String>,
    /// Clone source. If absent, the target path's configured Git remote is used.
    pub remote_url: Option<String>,
    /// Git remote name in the target checkout.
    pub remote: String,
    /// Main branch of this target.
    pub main: String,
    /// Optional target-specific integration checkout under the pump home.
    pub integration_repo: Option<String>,
    /// Target-specific merge behavior. Omitted fields inherit the legacy
    /// workspace-level merge settings when loaded through `target_configs`.
    pub merge: Option<MergeConfig>,
    /// Target-specific rung commands. A rung set here wins over the
    /// workspace-level `[pump.rungs]` one.
    #[serde(skip_serializing_if = "RungsConfig::is_empty")]
    pub rungs: RungsConfig,
}

impl Default for PumpTarget {
    fn default() -> Self {
        Self {
            path: String::from("."),
            github_repo: None,
            remote_url: None,
            remote: String::from("origin"),
            main: String::from("main"),
            integration_repo: None,
            merge: None,
            rungs: RungsConfig::default(),
        }
    }
}

/// Board pass settings (the `[pump.board]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BoardConfig {
    /// Senders of the pump's own replies: the first names the default
    /// `reply_to`. Not an intake filter; a need is planned exactly when it has
    /// an open pump record (`vivi pump send --need <handle>`).
    pub from: Vec<String>,
    /// Retired intake cutoff. Accepted only so `validate` can name the
    /// replacement; setting it is an error.
    #[serde(rename = "since", skip_serializing)]
    pub retired_since: Option<String>,
    /// Retired approval mark. Accepted only so `validate` can name the
    /// replacement; setting it is an error.
    #[serde(rename = "mark", skip_serializing)]
    pub retired_mark: Option<String>,
    /// Seconds between board passes.
    pub every: u64,
    /// Concurrent planners.
    pub planners_max: u32,
    /// Seconds a planner has to write `plans/<need>.json`.
    pub plan_timeout: u64,
    /// `--to` of thread replies; absent means the first of `from`.
    pub reply_to: Option<String>,
    /// Run `vivi sync pull` at the start of a pass.
    pub pull: bool,
    /// After replying: detached `vivi sync push --since <push_since> --drain`.
    pub push_since: String,
}

impl Default for BoardConfig {
    fn default() -> Self {
        Self {
            from: Vec::new(),
            retired_since: None,
            retired_mark: None,
            every: 300,
            planners_max: 3,
            plan_timeout: 1800,
            reply_to: None,
            pull: true,
            push_since: "30m".into(),
        }
    }
}

/// Proof-validation settings (the `[pump.proof]` table).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProofConfig {
    /// Extra starting binaries proof commands may use, matched on basename.
    pub allow: Vec<String>,
}

/// Plan-check settings (the `[pump.plan]` table).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PlanConfig {
    /// Proof rules a planned unit must satisfy (`[[pump.plan.proof_rules]]`).
    pub proof_rules: Vec<ProofRule>,
}

impl PlanConfig {
    /// True when no key of the table is set, so it is left out of a rewrite.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.proof_rules.is_empty()
    }
}

/// One `[[pump.plan.proof_rules]]` entry: a unit whose write scope matches
/// `scope` must carry a proof command matching `proof`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProofRule {
    /// Glob over a unit's write scope (`*`, `?`, `[..]` within a path segment,
    /// `**` across segments). A unit matches when one of its write-scope
    /// entries could name a path the glob covers.
    pub scope: String,
    /// Regular expression one of the unit's proof commands must match.
    pub proof: String,
    /// Why the rule exists; the planner reads it when its plan is rejected.
    /// Empty falls back to a message naming the scope and the proof.
    pub reason: String,
}

impl ProofRule {
    /// The proof regex, or the compile error text.
    ///
    /// # Errors
    /// The `regex` crate's message when `proof` is not a valid pattern.
    pub fn pattern(&self) -> Result<regex::Regex, String> {
        regex::Regex::new(&self.proof).map_err(|error| error.to_string())
    }

    /// The reason a unit that matches `scope` but has no matching proof is
    /// rejected.
    #[must_use]
    pub fn reason_text(&self) -> String {
        if self.reason.trim().is_empty() {
            format!(
                "write scope `{}` needs a proof command matching `{}`",
                self.scope, self.proof
            )
        } else {
            self.reason.trim().to_owned()
        }
    }
}

/// One dispatch lane: a model and the slots it may hold.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Lane {
    /// Model dispatched on this lane.
    pub model: String,
    /// Concurrent units on this lane.
    pub cap: u32,
}

/// Dispatch settings (the `[pump.dispatch]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DispatchConfig {
    /// Dispatch lanes. Empty is allowed when a role assigns a pool
    /// (capacity then comes from that pool's `slots` / `caps`); otherwise
    /// `validate` still requires at least one lane.
    pub lanes: Vec<Lane>,
    /// Model used when attempts reach `max_attempts - 1`.
    pub last_resort: String,
    /// Attempts before a unit is stuck.
    pub max_attempts: u32,
    /// `--budget` of a unit dispatch, in seconds.
    pub budget: u64,
    /// Per-thinking-step budget overrides (the `[pump.dispatch.step_budgets]`
    /// table). Absent keys fall back to `budget` (`envelope-fix` to 300);
    /// the table is omitted when empty.
    #[serde(default, skip_serializing_if = "StepBudgets::is_empty")]
    pub step_budgets: StepBudgets,
    /// Transient-class failures (host/harness, shared-check timeout,
    /// generated-file conflict) are retried without counting toward
    /// `max_attempts`, at most this many times; 0 disables.
    pub transient_max: u32,
    /// Seconds before the first transient retry; doubles on each further
    /// transient retry.
    pub transient_backoff: u64,
    /// Running longer than `budget` plus this means split.
    pub overdue_grace: u64,
    /// Units started per tick at most.
    pub per_tick: u32,
    /// Pool dispatches (task send plus `pool dispatch`) run at once within a
    /// tick, for both unit starts and durable role jobs. 1 starts them one
    /// after another.
    pub parallel: u32,
    /// `--from` of a unit dispatch; absent means `main`.
    pub from: Option<String>,
    /// Unit kinds (`[pump.dispatch.kinds.<name>]`): a unit's `tools` value
    /// names its kind. Without an assigned hand-role pool, the kind names the
    /// pools the unit goes to; `max` and `skip_proof` also apply to role routes.
    pub kinds: BTreeMap<String, Kind>,
    /// Kind of a unit with no `tools` value. Required when `kinds` is non-empty
    /// for legacy pool routing; role-pool routing can omit it.
    pub default_kind: String,
}

/// One unit kind (a `[pump.dispatch.kinds.<name>]` table).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Kind {
    /// Pools units of this kind are dispatched to by legacy routing; each must
    /// name a `[pools.*]` table. A unit goes to the listed pool with the most
    /// free slots (ties: first listed). Ignored when the hand role has a pool.
    pub pools: Vec<String>,
    /// Units of this kind allowed in flight at once; absent = no cap. While
    /// this many are inflight, a further unit of the kind waits (skipped,
    /// not failed).
    pub max: Option<u32>,
    /// Units of this kind skip their own proof commands at merge; only the
    /// shared checks run for them.
    pub skip_proof: bool,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            lanes: Vec::new(),
            last_resort: String::new(),
            max_attempts: 3,
            budget: 600,
            step_budgets: StepBudgets::default(),
            transient_max: 2,
            transient_backoff: 120,
            overdue_grace: 900,
            per_tick: 6,
            parallel: 6,
            from: None,
            kinds: BTreeMap::new(),
            default_kind: String::new(),
        }
    }
}

/// Merge settings (the `[pump.merge]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MergeConfig {
    /// Format command run after merging.
    pub format: String,
    /// Seconds a claimed merge may take.
    pub remote_limit: u64,
    /// Files the merge lane regenerates instead of resolving by hand.
    pub generated: Vec<Generated>,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            format: String::new(),
            remote_limit: 1800,
            generated: Vec::new(),
        }
    }
}

impl MergeConfig {
    /// Every declared generated path, in declaration order, across all
    /// entries.
    #[must_use]
    pub fn generated_paths(&self) -> Vec<String> {
        self.generated
            .iter()
            .flat_map(|entry| entry.paths.iter().cloned())
            .collect()
    }
}

/// One set of generated files the merge lane rebuilds from source of truth
/// instead of resolving by hand (a `[[pump.merge.generated]]` array entry).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Generated {
    /// Repository-relative paths of the generated files; globs are allowed
    /// and match like a unit's write scope.
    pub paths: Vec<String>,
    /// Shell command that rebuilds the files from source of truth; the merge
    /// lane runs it in the repository root.
    pub regenerate: String,
    /// Substrings of a failing check's output meaning the generated files are
    /// stale rather than the code broken.
    pub drift: Vec<String>,
}

/// An absolute path stands alone; a relative one joins `project`. `~` is not
/// expanded.
fn resolve(project: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project.join(path)
    }
}

/// Whether `name` is one of the mailspace pool names.
fn known(pools: &[String], name: &str) -> bool {
    pools.iter().any(|pool| pool == name)
}
