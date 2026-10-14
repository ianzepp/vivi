//! Pump configuration (the `[pump]` table of `mailspace.toml`).

use crate::VivariumError;
use std::path::{Path, PathBuf};

/// Pump settings (the `[pump]` table in `mailspace.toml`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PumpConfig {
    /// Pump home directory; relative paths join the project root and `~` is
    /// not expanded. Default `<project>/.vivi/pump`.
    pub home: Option<String>,
    /// The integration clone; cloned from `remote` when absent. Default
    /// `<home>/repo`.
    pub repo: Option<String>,
    /// Git remote the integration branch and main live on.
    pub remote: String,
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
    /// generic text.
    pub hand_rules: Option<String>,
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
            branch: String::new(),
            main: "main".into(),
            promote: true,
            tick: 20,
            shell_prefix: String::new(),
            hand_rules: None,
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
    /// Pool for units with `tools = "plain"`; must name a `[pools.*]` table.
    pub pool_plain: String,
    /// Pool for units with `tools = "full"`; must name a `[pools.*]` table.
    pub pool_full: String,
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
            pool_plain: String::new(),
            pool_full: String::new(),
        }
    }
}

/// Merge settings (the `[pump.merge]` table).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MergeConfig {
    /// Jobs merged together at most.
    pub batch: u32,
    /// Format command run after merging.
    pub format: String,
    /// Shared checks run after every merge, before the units' own proof.
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
    /// Gate command run by a read-only gate worker.
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
        }
    }
}

impl PumpConfig {
    /// Check the config against itself and the known pool names.
    ///
    /// `pools` holds the names of the mailspace's `[pools.<name>]` tables.
    ///
    /// # Errors
    /// [`VivariumError::Config`] naming every violated key when a rule fails.
    pub fn validate(&self, pools: &[String]) -> Result<(), VivariumError> {
        let mut bad: Vec<String> = Vec::new();
        if self.branch.is_empty() {
            bad.push("[pump] branch must not be empty".into());
        }
        if self.tick == 0 {
            bad.push("[pump] tick must be >= 1".into());
        }
        self.validate_dispatch(pools, &mut bad);
        self.validate_merge_and_gate(pools, &mut bad);
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
        for (key, name) in [
            ("[pump.dispatch] pool_plain", &self.dispatch.pool_plain),
            ("[pump.dispatch] pool_full", &self.dispatch.pool_full),
        ] {
            if !name.is_empty() && !known(pools, name) {
                bad.push(format!("{key} names unknown pool `{name}`"));
            }
        }
    }

    fn validate_merge_and_gate(&self, pools: &[String], bad: &mut Vec<String>) {
        if self.merge.batch == 0 {
            bad.push("[pump.merge] batch must be >= 1".into());
        }
        if self.gate.every_merges == 0 {
            bad.push("[pump.gate] every_merges must be >= 1".into());
        }
        for name in &self.gate.pools {
            if !known(pools, name) {
                bad.push(format!("[pump.gate] pools names unknown pool `{name}`"));
            }
        }
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
