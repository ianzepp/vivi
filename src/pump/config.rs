//! Pump configuration (the `[pump]` table of `mailspace.toml`).

use crate::VivariumError;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
    /// Need handles dispatched and merged ahead of everything else, one per
    /// line, `#` comments. A relative value resolves against `home`. Default
    /// `priority.txt`; the file may be absent.
    pub priority_file: String,
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
    /// Dispatch settings (the `[pump.dispatch]` table).
    pub dispatch: DispatchConfig,
    /// Merge settings (the `[pump.merge]` table).
    pub merge: MergeConfig,
    /// Gate settings (the `[pump.gate]` table).
    pub gate: GateConfig,
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
            priority_file: "priority.txt".into(),
            fix_prefix: "pumpfix".into(),
            from: "operator".into(),
            to: "hand".into(),
            reply_from: "mind".into(),
            board: BoardConfig::default(),
            planner: PlannerConfig::default(),
            dispatch: DispatchConfig::default(),
            merge: MergeConfig::default(),
            gate: GateConfig::default(),
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
    /// Senders whose open needs are plannable.
    pub from: Vec<String>,
    /// Needs dated before this are skipped, unless marked or re-planned.
    pub since: String,
    /// Approval mark: a marked open need is plannable whatever sender or date.
    pub mark: String,
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
            since: String::new(),
            mark: "pump".into(),
            every: 300,
            planners_max: 3,
            plan_timeout: 1800,
            reply_to: None,
            pull: true,
            push_since: "30m".into(),
        }
    }
}

/// Planner settings (the `[pump.planner]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PlannerConfig {
    /// Planner command; its stdin is the prompt file.
    pub run: String,
    /// Charter file substituted for `{charter}` in `run`.
    pub charter: String,
    /// Seconds a split planner has to answer before the unit is stuck.
    pub split_timeout: u64,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            run: String::new(),
            charter: String::new(),
            split_timeout: 1500,
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
    /// Dispatch lanes; empty is rejected by `validate`.
    pub lanes: Vec<Lane>,
    /// Model used when attempts reach `max_attempts - 1`.
    pub last_resort: String,
    /// Attempts before a unit is stuck.
    pub max_attempts: u32,
    /// `--budget` of a unit dispatch, in seconds.
    pub budget: u64,
    /// Running longer than `budget` plus this means split.
    pub overdue_grace: u64,
    /// Units started per tick at most.
    pub per_tick: u32,
    /// `--from` of a unit dispatch; absent means `branch`.
    pub from: Option<String>,
    /// Unit kinds (`[pump.dispatch.kinds.<name>]`): a unit's `tools` value
    /// names its kind, and the kind names the pools the unit goes to.
    pub kinds: BTreeMap<String, Kind>,
    /// Kind of a unit with no `tools` value. Required (and must be a defined
    /// kind) when `kinds` is non-empty.
    pub default_kind: String,
}

/// One unit kind (a `[pump.dispatch.kinds.<name>]` table).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Kind {
    /// Pools units of this kind are dispatched to; each must name a
    /// `[pools.*]` table. Required, non-empty. A unit goes to the listed pool
    /// with the most free slots (ties: first listed).
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
            overdue_grace: 900,
            per_tick: 6,
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
    /// Seconds per check command.
    pub proof_timeout: u64,
    /// Seconds a remote merger has to claim a batch before this host merges.
    pub remote_wait: u64,
    /// Seconds a claimed batch may take.
    pub remote_limit: u64,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            batch: 8,
            lanes: 1,
            format: String::new(),
            shared_checks: Vec::new(),
            proof_timeout: 300,
            remote_wait: 90,
            remote_limit: 1800,
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
    /// A gate is due after this many merges.
    pub every_merges: u32,
    /// A gate is also due after this many seconds with new merges.
    pub every_seconds: u64,
    /// Pools a gate worker may use; each must name a `[pools.*]` table.
    pub pools: Vec<String>,
    /// Gate worker model.
    pub model: String,
    /// Gate worker budget, in seconds.
    pub budget: u64,
    /// Directory an external runner drops `<tip>.json` gate results in;
    /// relative paths join the project root. Absent means pool gates only.
    pub results_dir: Option<String>,
    /// With `results_dir` set, seconds a due gate waits for an external
    /// result before a pool gate is dispatched instead. Must be >= 1 then.
    pub results_wait_seconds: u64,
    /// File of test names dropped from every gate's failing set, one per
    /// line, `#` comments; relative paths join the project root.
    pub ignore_file: Option<String>,
    /// How many times a name that still fails after every unit of its owner
    /// settled is handed to a fresh gate-fix planner. 0 never re-plans.
    pub max_replans: u32,
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
            ignore_file: None,
            max_replans: 2,
        }
    }
}

impl PumpConfig {
    /// Check the config against itself and the known pool names.
    ///
    /// `pools` holds the names of the mailspace's `[pools.<name>]` tables.
    ///
    /// `project` is the base a relative `hand_rules` path resolves against.
    ///
    /// # Errors
    /// [`VivariumError::Config`] naming every violated key when a rule fails.
    pub fn validate(&self, project: &Path, pools: &[String]) -> Result<(), VivariumError> {
        let mut bad: Vec<String> = Vec::new();
        if self.committer_name.trim().is_empty() {
            bad.push("[pump] committer_name must not be empty".into());
        }
        if self.committer_email.trim().is_empty() {
            bad.push("[pump] committer_email must not be empty".into());
        }
        if let Some(path) = self.hand_rules_path(project)
            && let Err(err) = std::fs::read_to_string(&path)
        {
            bad.push(format!(
                "[pump] hand_rules `{}` cannot be read: {err}",
                path.display()
            ));
        }
        if self.tick == 0 {
            bad.push("[pump] tick must be >= 1".into());
        }
        self.validate_dispatch(pools, &mut bad);
        if self.targets.is_empty() {
            if self.branch.is_empty() {
                bad.push("[pump] branch must not be empty".into());
            }
            self.validate_merge_and_gate(pools, &mut bad);
        } else {
            let mut target_paths = BTreeSet::new();
            for (id, target) in self.target_configs() {
                let normalized: PathBuf = Path::new(&target.path).components().collect();
                if !target_paths.insert(normalized) {
                    bad.push(format!(
                        "[pump.targets.{id}] path duplicates another configured repository target"
                    ));
                }
                validate_target(&id, &target, pools, &mut bad);
            }
        }
        if (self.board.from.iter().any(|from| !from.is_empty()) || !self.board.mark.is_empty())
            && self.planner.run.is_empty()
        {
            bad.push(
                "[pump.planner] run must not be empty while [pump.board] from or mark is set"
                    .into(),
            );
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(VivariumError::Config(bad.join("; ")))
        }
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

    /// The priority file: `priority_file`, a relative value joining `home`.
    #[must_use]
    pub fn priority_path(&self, project: &Path) -> PathBuf {
        resolve(&self.home_dir(project), &self.priority_file)
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

    fn validate_dispatch(&self, pools: &[String], bad: &mut Vec<String>) {
        if self.dispatch.lanes.is_empty() {
            bad.push("[pump.dispatch] lanes must not be empty".into());
        }
        for lane in &self.dispatch.lanes {
            if lane.cap == 0 {
                bad.push(format!(
                    "[pump.dispatch] lanes: lane `{}` needs cap >= 1",
                    lane.model
                ));
            }
            if lane.model.is_empty() {
                bad.push("[pump.dispatch] lanes: lane model must not be empty".into());
            }
        }
        if self.dispatch.max_attempts == 0 {
            bad.push("[pump.dispatch] max_attempts must be >= 1".into());
        }
        if self.dispatch.budget == 0 {
            bad.push("[pump.dispatch] budget must be >= 1".into());
        }
        if self.dispatch.per_tick == 0 {
            bad.push("[pump.dispatch] per_tick must be >= 1".into());
        }
        self.validate_kinds(pools, bad);
    }

    fn validate_kinds(&self, pools: &[String], bad: &mut Vec<String>) {
        for (name, kind) in &self.dispatch.kinds {
            if kind.pools.is_empty() {
                bad.push(format!(
                    "[pump.dispatch.kinds.{name}] pools must not be empty"
                ));
            }
            for pool in &kind.pools {
                if !known(pools, pool) {
                    bad.push(format!(
                        "[pump.dispatch.kinds.{name}] pools names unknown pool `{pool}`"
                    ));
                }
            }
            if kind.max == Some(0) {
                bad.push(format!("[pump.dispatch.kinds.{name}] max must be >= 1"));
            }
        }
        if !self.dispatch.kinds.is_empty() {
            let default = &self.dispatch.default_kind;
            if default.is_empty() {
                bad.push("[pump.dispatch] default_kind must be set when kinds is non-empty".into());
            } else if !self.dispatch.kinds.contains_key(default) {
                bad.push(format!(
                    "[pump.dispatch] default_kind names undefined kind `{default}`"
                ));
            }
        }
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

    fn validate_merge_and_gate(&self, pools: &[String], bad: &mut Vec<String>) {
        if self.merge.batch == 0 {
            bad.push("[pump.merge] batch must be >= 1".into());
        }
        if self.merge.lanes == 0 {
            bad.push("[pump.merge] lanes must be >= 1".into());
        }
        if self.promote && !self.main.is_empty() && self.merge.shared_checks.is_empty() {
            bad.push(
                "[pump.merge] shared_checks must not be empty while promotion is enabled \
                 (promote = true and main set)"
                    .into(),
            );
        }
        if self.gate.command.trim().is_empty() {
            bad.push("[pump.gate] command must not be empty".into());
        }
        if self.gate.every_merges == 0 {
            bad.push("[pump.gate] every_merges must be >= 1".into());
        }
        if self.gate.results_dir.is_some() && self.gate.results_wait_seconds == 0 {
            bad.push("[pump.gate] results_dir needs results_wait_seconds >= 1".into());
        }
        for name in &self.gate.pools {
            if !known(pools, name) {
                bad.push(format!("[pump.gate] pools names unknown pool `{name}`"));
            }
        }
    }
}

fn validate_target(id: &str, target: &PumpTarget, pools: &[String], bad: &mut Vec<String>) {
    let prefix = format!("[pump.targets.{id}]");
    if !valid_target_id(id) {
        bad.push(format!(
            "{prefix} id must use lowercase letters, digits and hyphens"
        ));
    }
    let path = Path::new(&target.path);
    if target.path.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        bad.push(format!(
            "{prefix} path must be a workspace-relative path without `..`"
        ));
    }
    if target.branch.trim().is_empty() {
        bad.push(format!("{prefix} branch must not be empty"));
    }
    if target.github_repo.as_deref().is_some_and(str::is_empty) {
        bad.push(format!("{prefix} github_repo must not be empty when set"));
    }
    let merge = target.merge.as_ref();
    if merge.is_some_and(|merge| merge.batch == 0) {
        bad.push(format!("{prefix}.merge batch must be >= 1"));
    }
    if merge.is_some_and(|merge| merge.lanes == 0) {
        bad.push(format!("{prefix}.merge lanes must be >= 1"));
    }
    if target.promote
        && !target.main.is_empty()
        && merge.is_some_and(|merge| merge.shared_checks.is_empty())
    {
        bad.push(format!(
            "{prefix}.merge shared_checks must not be empty while promotion is enabled"
        ));
    }
    if let Some(gate) = &target.gate {
        validate_target_gate(id, gate, pools, bad);
    } else {
        bad.push(format!("{prefix}.gate command must not be empty"));
    }
}

fn validate_target_gate(id: &str, gate: &GateConfig, pools: &[String], bad: &mut Vec<String>) {
    let prefix = format!("[pump.targets.{id}.gate]");
    if gate.command.trim().is_empty() {
        bad.push(format!("{prefix} command must not be empty"));
    }
    if gate.every_merges == 0 {
        bad.push(format!("{prefix} every_merges must be >= 1"));
    }
    if gate.results_dir.is_some() && gate.results_wait_seconds == 0 {
        bad.push(format!(
            "{prefix} results_dir needs results_wait_seconds >= 1"
        ));
    }
    for name in &gate.pools {
        if !known(pools, name) {
            bad.push(format!("{prefix} pools names unknown pool `{name}`"));
        }
    }
}

fn valid_target_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
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
