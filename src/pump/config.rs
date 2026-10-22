//! Pump configuration (the `[pump]` table of `mailspace.toml`).

use crate::pump::alarm::AlarmConfig;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use self::config_workspace::PumpWorkspace;

#[path = "config_workspace.rs"]
pub mod config_workspace;
mod planner;
mod validate;

pub use planner::PlannerConfig;
pub use validate::{Findings, Missing};

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
    /// Git remote (name) the integration branch and main live on.
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
    /// The integration branch. Required: `validate` rejects an empty one.
    pub branch: String,
    /// Main branch merged into the integration branch each batch; empty
    /// disables merging main in and promotion.
    pub main: String,
    /// A clean gate fast-forwards main to the gated tip.
    pub promote: bool,
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
    /// Dispatch settings (the `[pump.dispatch]` table).
    pub dispatch: DispatchConfig,
    /// Merge settings (the `[pump.merge]` table).
    pub merge: MergeConfig,
    /// Lane RAM disk settings (the `[pump.workspace]` table).
    pub workspace: PumpWorkspace,
    /// Gate settings (the `[pump.gate]` table).
    pub gate: GateConfig,
    /// Stall alarm thresholds (the `[pump.alarm]` table).
    pub alarm: AlarmConfig,
    /// Need-acceptance settings (the `[pump.need]` table).
    pub need: NeedConfig,
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
            branch: String::new(),
            main: "main".into(),
            promote: true,
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
            dispatch: DispatchConfig::default(),
            merge: MergeConfig::default(),
            workspace: PumpWorkspace::default(),
            gate: GateConfig::default(),
            alarm: AlarmConfig::default(),
            need: NeedConfig::default(),
        }
    }
}

impl PumpConfig {
    /// Effective repository targets. Legacy single-repository fields become
    /// one `default` target; explicit targets inherit only shared merge/gate
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
                    branch: self.branch.clone(),
                    main: self.main.clone(),
                    promote: self.promote,
                    integration_repo: self.repo.clone(),
                    merge: Some(self.merge.clone()),
                    gate: Some(self.gate.clone()),
                    ..PumpTarget::default()
                },
            )]);
        }
        self.targets
            .iter()
            .map(|(id, target)| {
                let mut target = target.clone();
                target.merge.get_or_insert_with(|| self.merge.clone());
                target.gate.get_or_insert_with(|| self.gate.clone());
                (id.clone(), target)
            })
            .collect()
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
    pub fn for_target(&self, id: &str) -> Option<Self> {
        let target = self.target_configs().remove(id)?;
        let mut config = self.clone();
        config.repo = target.integration_repo;
        config.remote_url = target.remote_url;
        config.remote = target.remote;
        config.branch = target.branch;
        config.main = target.main;
        config.promote = target.promote;
        config.merge = target.merge.unwrap_or_default();
        config.gate = target.gate.unwrap_or_default();
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
    /// Integration branch for this target.
    pub branch: String,
    /// Main branch merged into the target integration branch.
    pub main: String,
    /// Whether a clean target gate promotes its branch to main.
    pub promote: bool,
    /// Optional target-specific integration checkout under the pump home.
    pub integration_repo: Option<String>,
    /// Target-specific merge behavior. Omitted fields inherit the legacy
    /// workspace-level merge settings when loaded through `target_configs`.
    pub merge: Option<MergeConfig>,
    /// Target-specific gate behavior. Omitted fields inherit the legacy
    /// workspace-level gate settings when loaded through `target_configs`.
    pub gate: Option<GateConfig>,
}

impl Default for PumpTarget {
    fn default() -> Self {
        Self {
            path: String::from("."),
            github_repo: None,
            remote_url: None,
            remote: String::from("origin"),
            branch: String::new(),
            main: String::from("main"),
            promote: true,
            integration_repo: None,
            merge: None,
            gate: None,
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
    /// `--from` of a unit dispatch; absent means `branch`.
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
            transient_max: 2,
            transient_backoff: 120,
            overdue_grace: 900,
            per_tick: 6,
            from: None,
            kinds: BTreeMap::new(),
            default_kind: String::new(),
        }
    }
}

/// Need-acceptance settings (the `[pump.need]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct NeedConfig {
    /// How many `block_range` repair units a finding may receive before the
    /// pump holds the branch and escalates. Default 1 preserves the previous
    /// one-repair-then-hold behavior.
    pub repair_rounds: u32,
}

impl Default for NeedConfig {
    fn default() -> Self {
        Self { repair_rounds: 1 }
    }
}

/// Merge settings (the `[pump.merge]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MergeConfig {
    /// Jobs merged together at most.
    pub batch: u32,
    /// Independent merge lanes per repository target, each with its own
    /// checkout and build directory. A lane takes the top-ranked delivered
    /// units when it is idle, so a red lane never blocks the others.
    pub lanes: u32,
    /// Format command run after merging.
    pub format: String,
    /// Shared checks run after every merge, before the units' own proof.
    /// `validate` rejects an empty list while promotion is enabled.
    pub shared_checks: Vec<String>,
    /// Extra checks for units whose write scope overlaps a glob. Each key is
    /// a write-scope glob matched like a unit's write scope (`overlaps`),
    /// each value the commands run after the shared checks and before the
    /// units' own proof, only for a batch or solo merge with an overlapping
    /// unit. A failure decides exactly like a shared check.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checks_for: BTreeMap<String, Vec<String>>,
    /// Seconds per check command.
    pub proof_timeout: u64,
    /// Optional wall-clock budget for quarantine repair units. When absent,
    /// the effective budget is `proof_timeout` plus repair headroom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_budget: Option<u64>,
    /// Seconds a remote merger has to claim a batch before this host merges.
    pub remote_wait: u64,
    /// Seconds a claimed batch may take.
    pub remote_limit: u64,
    /// Files the merge lane regenerates instead of resolving by hand.
    pub generated: Vec<Generated>,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            batch: 8,
            lanes: 1,
            format: String::new(),
            shared_checks: Vec::new(),
            checks_for: BTreeMap::new(),
            proof_timeout: 300,
            repair_budget: None,
            remote_wait: 90,
            remote_limit: 1800,
            generated: Vec::new(),
        }
    }
}

impl MergeConfig {
    /// Budget for a quarantine repair, with headroom above its failing proof.
    #[must_use]
    pub fn quarantine_repair_budget(&self) -> u64 {
        self.repair_budget
            .unwrap_or_else(|| self.proof_timeout.saturating_add(300))
    }

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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

impl Default for Generated {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            regenerate: String::new(),
            drift: Vec::new(),
        }
    }
}

/// Gate settings (the `[pump.gate]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GateConfig {
    /// Gate command run by a read-only gate worker. Required: `validate`
    /// rejects an empty one.
    pub command: String,
    /// Retired: accepted and ignored. A gate is due after every merge batch
    /// that lands (need e90b0221).
    pub every_merges: u32,
    /// Retired: accepted and ignored. A gate is due after every merge batch
    /// that lands (need e90b0221).
    pub every_seconds: u64,
    /// Ordered gate fallback pools after the assigned gate role's pool and its
    /// configured fallback chain; each must name a `[pools.*]` table.
    pub pools: Vec<String>,
    /// Gate worker model.
    pub model: String,
    /// Gate worker budget, in seconds.
    pub budget: u64,
    /// Directory an external runner drops `<tip>.json` gate results in;
    /// relative paths join the project root. Absent means pool gates only.
    pub results_dir: Option<String>,
    /// Seconds a due gate waits for an external result; >= 1 with results_dir.
    pub results_wait_seconds: u64,
    /// Seconds without a consumed external result after which a due gate stops
    /// waiting; `0` disables the rule. A runner that has produced nothing for
    /// this long is not running, so the pool gate goes out at once.
    pub results_idle_seconds: u64,
    /// Gate-ignored test names, one per line; `#` comments; relative paths resolve from project.
    pub ignore_file: Option<String>,
    /// Replans per name after owner units settle but the name still fails; 0 disables.
    pub max_replans: u32,
    /// Write HOLD-MERGE while owned reds remain unfixed.
    pub hold_on_red: bool,
    /// Need priority floor for bypassing HOLD-MERGE.
    pub hold_priority_floor: i64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            every_merges: 8,
            every_seconds: 1800,
            pools: Vec::new(),
            model: String::new(),
            budget: 1800,
            results_dir: None,
            results_wait_seconds: 0,
            results_idle_seconds: 0,
            ignore_file: None,
            max_replans: 2,
            hold_on_red: false,
            hold_priority_floor: 0,
        }
    }
}

impl PumpConfig {
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

    /// The external gate results directory, when configured.
    #[must_use]
    pub fn results_dir(&self, project: &Path) -> Option<PathBuf> {
        self.gate
            .results_dir
            .as_deref()
            .map(|dir| resolve(project, dir))
    }

    /// The gate ignore file, when configured.
    #[must_use]
    pub fn ignore_file(&self, project: &Path) -> Option<PathBuf> {
        self.gate
            .ignore_file
            .as_deref()
            .map(|file| resolve(project, file))
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

    /// `--from` of a unit dispatch: `dispatch.from`, else `branch`.
    #[must_use]
    pub fn dispatch_from(&self) -> &str {
        self.dispatch.from.as_deref().unwrap_or(&self.branch)
    }

    /// `--to` of thread replies: `board.reply_to`, else the first of
    /// `board.from`, else empty.
    #[must_use]
    pub fn reply_to(&self) -> String {
        self.board
            .reply_to
            .clone()
            .or_else(|| self.board.from.first().cloned())
            .unwrap_or_default()
    }

    /// The name of the kind a unit's `tools` value selects: `tools`, or
    /// `default_kind` when `tools` is empty.
    #[must_use]
    pub fn kind_name<'a>(&'a self, tools: &'a str) -> &'a str {
        if tools.is_empty() {
            &self.dispatch.default_kind
        } else {
            tools
        }
    }

    /// The kind a unit's `tools` value selects; `None` when it is not defined.
    #[must_use]
    pub fn kind_of(&self, tools: &str) -> Option<&Kind> {
        self.dispatch.kinds.get(self.kind_name(tools))
    }
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

/// Whether `name` is one of the mailspace's pool names.
fn known(pools: &[String], name: &str) -> bool {
    pools.iter().any(|pool| pool == name)
}
