//! `vivi pool`: remote lane pool.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum PoolSubcommand {
    /// Create and seed the missing slots of a pool up to its size
    Create(PoolCreateCommand),
    /// List the slots of the pool (or of every pool) from the provider
    Status(PoolStatusCommand),
    /// Destroy one slot or all of them
    Destroy(PoolDestroyCommand),
    /// Bring a pool to a number of slots: create and seed, or destroy free ones
    Scale(PoolScaleCommand),
    /// Hand a task to a free slot and return at once
    Dispatch(PoolDispatchCommand),
    /// Block until a dispatched task finishes (in any pool, or --pool), then collect it
    Wait(PoolWaitCommand),
    /// Collect finished dispatches, stop overdue ones, free their slots (all pools, or --pool)
    Reconcile(PoolReconcileCommand),
    /// Check the pools, their providers and repositories, naming the fix for each finding; read-only unless --test (which needs --pool when there are several)
    Doctor(PoolDoctorCommand),
    /// Reclaim disposable build output and recycle finished work lanes
    Clean(PoolCleanCommand),
    /// Reset a free slot onto a freshly fetched origin branch
    Reset(PoolResetCommand),
    /// List configured pools
    List(PoolListCommand),
    /// Show one pool's configured (or resolved) settings
    Show(PoolShowCommand),
    /// Add a pool table
    Add(PoolAddCommand),
    /// Set one key on a pool table
    Set(PoolConfigSetCommand),
    /// Remove one key from a pool table
    Unset(PoolUnsetCommand),
    /// Remove a pool table
    Remove(PoolRemoveCommand),
    /// Set or clear a pool band choice
    Bands {
        #[command(subcommand)]
        command: PoolBandsCommand,
    },
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct PoolDoctorCommand {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Print only result lines in a fixed format, for scripts and agents: no
    /// progress, no heartbeats (agents: pass this)
    #[arg(long, conflicts_with = "json")]
    pub porcelain: bool,

    /// Also run a live test of the provider and of Vivi's pool machinery:
    /// creates two small throwaway slots (billable on Fly, free on Docker
    /// and SSH, for a few minutes), runs stand-in jobs against an embedded source tree, and
    /// destroys them. On a RunPod pool it rents one GPU pod instead and
    /// refuses to run without --allow-spend
    #[arg(long)]
    pub test: bool,

    /// With --test on a pool whose provider rents billable GPU machines
    /// (RunPod): agree that the test spends real money. Without it the test is
    /// refused, naming the price
    #[arg(long, requires = "test")]
    pub allow_spend: bool,

    /// With --test, add a wait that runs past six minutes (the provider token
    /// lifetime) and still collects; adds about seven minutes
    #[arg(long, requires = "test")]
    pub long: bool,

    /// Pool name (`[pools.<name>]`); without it, every pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolDispatchCommand {
    /// Repository target from the workspace pump configuration
    #[arg(long)]
    pub target: Option<String>,

    /// Another repository target to check out read-only beside the job's own,
    /// repeatable (a planner reads every target). Read-only jobs only; each
    /// is placed at `<slot>/targets/<key>/repo` and named in the brief
    #[arg(long, value_name = "TARGET", requires = "read_only")]
    pub also_target: Vec<String>,

    /// Task handle; its body is the brief
    pub task: String,

    /// Need provenance supplied by the pump for monitor and dispatch receipts.
    /// This metadata does not change the role or its permissions.
    #[arg(long, hide = true)]
    pub need: Option<String>,

    /// Work kind supplied by the pump for monitor and dispatch receipts.
    /// This metadata does not change the role or its permissions.
    #[arg(long, hide = true)]
    pub kind: Option<String>,

    /// Model slug; must be one of `[pools.<name>.harness].models`
    /// (default: the model the task's role band picks from the pool's band table)
    #[arg(long)]
    pub model: Option<String>,

    /// Commit or branch to start from (default: a `from:` line in the task body)
    #[arg(long)]
    pub from: Option<String>,

    /// Branch to push to (default: a `branch:` line in the task body)
    #[arg(long)]
    pub branch: Option<String>,

    /// Publish the runner-verified candidate to this remote heads ref after a clean agent exit.
    #[arg(long, value_name = "REF", conflicts_with = "read_only")]
    pub publish_ref: Option<String>,

    /// Require the candidate commit to equal this full SHA before publishing (promotion jobs).
    #[arg(
        long,
        value_name = "SHA",
        requires = "publish_ref",
        conflicts_with = "read_only"
    )]
    pub publish_expect: Option<String>,

    /// Require this commit to be included in the candidate and published target.
    #[arg(
        long,
        value_name = "SHA",
        requires = "publish_ref",
        conflicts_with = "read_only"
    )]
    pub publish_contains: Option<String>,

    /// Delete an owned need branch only if it still matches --delete-expect.
    #[arg(long, value_name = "REF", requires = "delete_expect", conflicts_with_all = ["read_only", "publish_ref"])]
    pub delete_ref: Option<String>,

    /// Exact expected tip of the owned need branch to delete.
    #[arg(
        long,
        value_name = "SHA",
        requires = "delete_ref",
        conflicts_with = "read_only"
    )]
    pub delete_expect: Option<String>,

    /// Extra ref the job may read besides where it starts, repeatable, at
    /// most 8 (default: every `with: <ref>` line of the task body, in order;
    /// flags win when both are given). Each is resolved to a commit before a
    /// slot is claimed
    #[arg(long, value_name = "REF")]
    pub with: Vec<String>,

    /// Publish nothing: check `--from` out detached, run the agent, then
    /// discard whatever it changed (listed on the result); no commit, no
    /// push, a token that cannot push. Excludes `--branch`
    #[arg(long, conflicts_with_all = ["branch", "allow_default_branch"])]
    pub read_only: bool,

    /// Hard time limit for the agent, in seconds (default 1800)
    #[arg(long)]
    pub budget: Option<u64>,

    /// Run this shell command as the harness instead of a model: no model is
    /// chosen, no role prompt or bridge grant is made, and the command's exit
    /// code is the job's result
    #[arg(long, value_name = "CMD", conflicts_with = "model")]
    pub command: Option<String>,

    /// Allow pushing to the repository's default branch (refused otherwise)
    #[arg(long)]
    pub allow_default_branch: bool,

    /// Seconds a newly rented pod may take to answer on SSH before it is
    /// terminated and the next GPU type is tried (30 to 1800; default: the
    /// pool's `runpod.ready_timeout`, itself 120). Only for a pool whose
    /// slots are made per dispatch (a `RunPod` pool)
    #[arg(long)]
    pub ready_timeout: Option<u64>,

    /// Print only result lines in a fixed format, for scripts and agents: no
    /// progress, no heartbeats (agents: pass this)
    #[arg(long)]
    pub porcelain: bool,

    /// Pool name (`[pools.<name>]`); optional when the mailspace has one pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolWaitCommand {
    /// Task handles to wait on (default: every open dispatch)
    pub tasks: Vec<String>,

    /// Give up after this many seconds and exit nonzero
    #[arg(long, default_value_t = 3600)]
    pub timeout: u64,

    /// Print only result lines in a fixed format, for scripts and agents: no
    /// progress, no heartbeats (agents: pass this)
    #[arg(long)]
    pub porcelain: bool,

    /// Pool name (`[pools.<name>]`); without it, every pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolReconcileCommand {
    /// Print only result lines in a fixed format, for scripts and agents: no
    /// progress, no heartbeats (agents: pass this)
    #[arg(long)]
    pub porcelain: bool,

    /// Pool name (`[pools.<name>]`); without it, every pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolCreateCommand {
    /// Pool size to reach (default and maximum: the pool's configured `slots`)
    #[arg(long)]
    pub slots: Option<u32>,

    /// Commit, branch, or tag to seed at (default: the repo's default branch tip)
    #[arg(long)]
    pub base: Option<String>,

    /// Give up waiting on a slot's seed after this many seconds
    #[arg(long, default_value_t = 900)]
    pub timeout: u64,

    /// Print only result lines in a fixed format, for scripts and agents: no
    /// progress, no heartbeats (agents: pass this)
    #[arg(long)]
    pub porcelain: bool,

    /// Pool name (`[pools.<name>]`); optional when the mailspace has one pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolStatusCommand {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Print only result lines in a fixed format, for scripts and agents: no
    /// progress, no heartbeats (agents: pass this)
    #[arg(long, conflicts_with = "json")]
    pub porcelain: bool,

    /// Pool name (`[pools.<name>]`); without it, every pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolScaleCommand {
    /// Number of slots to bring the pool to (at most its configured `slots`; 0 empties it of free slots)
    pub target: u32,

    /// Confirm destroying free slots (required when scaling down)
    #[arg(long)]
    pub yes: bool,

    /// Commit, branch, or tag new slots seed at (default: the repo's default branch tip)
    #[arg(long)]
    pub base: Option<String>,

    /// Give up waiting on a new slot's seed after this many seconds
    #[arg(long, default_value_t = 900)]
    pub timeout: u64,

    /// Print only result lines in a fixed format, for scripts and agents: no
    /// progress, no heartbeats (agents: pass this)
    #[arg(long)]
    pub porcelain: bool,

    /// Pool name (`[pools.<name>]`); optional when the mailspace has one pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolCleanCommand {
    /// Reclaim from a serialized request written by a dispatched cleanup job
    #[arg(long, conflicts_with_all = ["dry_run", "pool", "base"])]
    pub after_merged_request: Option<PathBuf>,

    /// Report reclaimable space and stay put
    #[arg(long)]
    pub dry_run: bool,

    /// Print only result lines in a fixed format, for scripts and agents
    #[arg(long)]
    pub porcelain: bool,

    /// Pool name (`[pools.<name>]`); without it, every pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Origin branch to restore recycled work lanes onto (default: [pump] main)
    #[arg(long)]
    pub base: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolResetCommand {
    /// Slot name or machine id
    pub slot: Option<String>,

    /// Origin branch to fetch (default: [pump] main)
    #[arg(long)]
    pub base: Option<String>,

    /// Print only result lines in a fixed format, for scripts and agents
    #[arg(long)]
    pub porcelain: bool,

    /// Reset every free slot in this pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("target").required(true).args(["slot", "all"])))]
pub struct PoolDestroyCommand {
    /// Slot name or machine id to destroy
    #[arg(long)]
    pub slot: Option<String>,

    /// Destroy every slot in the pool
    #[arg(long)]
    pub all: bool,

    /// Confirm the destruction
    #[arg(long)]
    pub yes: bool,

    /// Pool name (`[pools.<name>]`); optional when the mailspace has one pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolListCommand {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Print only result lines in a fixed format
    #[arg(long, conflicts_with = "json")]
    pub porcelain: bool,

    /// Project root that owns .vivi/
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolShowCommand {
    /// Pool name
    pub name: String,

    /// Include live free/total slot counts
    #[arg(long)]
    pub resolved: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Print only result lines in a fixed format
    #[arg(long, conflicts_with = "json")]
    pub porcelain: bool,

    /// Project root that owns .vivi/
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolAddCommand {
    /// Pool name (`[pools.<name>]`)
    pub name: String,

    /// Machine host: local, docker, fly, ssh or runpod
    #[arg(long)]
    pub provider: String,

    /// Maximum pool size
    #[arg(long)]
    pub slots: u32,

    /// Local Git repository this pool worktrees
    #[arg(long)]
    pub worktree_of: Option<String>,

    /// GitHub owner/name
    #[arg(long)]
    pub repo: Option<String>,

    /// Container image
    #[arg(long)]
    pub image: Option<String>,

    /// Local or SSH slot root
    #[arg(long)]
    pub root: Option<String>,

    /// Project root that owns .vivi/
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolConfigSetCommand {
    /// Pool name
    pub name: String,

    /// Key, possibly dotted (`local.worktree_of`)
    pub key: String,

    /// Value (string, bool, integer, or TOML array)
    pub value: String,

    /// Project root that owns .vivi/
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolUnsetCommand {
    /// Pool name
    pub name: String,

    /// Key, possibly dotted
    pub key: String,

    /// Project root that owns .vivi/
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolRemoveCommand {
    /// Pool name
    pub name: String,

    /// Confirm the removal
    #[arg(long)]
    pub yes: bool,

    /// Remove even when roles or open dispatches still name the pool
    #[arg(long)]
    pub force: bool,

    /// Project root that owns .vivi/
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum PoolBandsCommand {
    /// Set the serving model for a band
    Set {
        /// Pool name
        pool: String,
        /// Band key (`P0`–`P5`)
        band: String,
        /// Model slug
        model: String,
        /// Reasoning effort (default `xhigh`)
        reasoning: Option<String>,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Remove a band from a pool
    Unset {
        /// Pool name
        pool: String,
        /// Band key (`P0`–`P5`)
        band: String,
        /// Project root that owns .vivi/
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
