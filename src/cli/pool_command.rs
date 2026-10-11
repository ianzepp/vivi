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
    /// destroys them
    #[arg(long)]
    pub test: bool,

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
    /// Task handle; its body is the brief
    pub task: String,

    /// Model slug; must be one of `[pools.<name>.harness].models`
    #[arg(long)]
    pub model: String,

    /// Commit or branch to start from (default: a `from:` line in the task body)
    #[arg(long)]
    pub from: Option<String>,

    /// Branch to push to (default: a `branch:` line in the task body)
    #[arg(long)]
    pub branch: Option<String>,

    /// Publish nothing: check `--from` out detached, run the agent, then
    /// discard whatever it changed (listed on the result); no commit, no
    /// push, a token that cannot push. Excludes `--branch`
    #[arg(long, conflicts_with_all = ["branch", "allow_default_branch"])]
    pub read_only: bool,

    /// Hard time limit for the agent, in seconds (default 1800)
    #[arg(long)]
    pub budget: Option<u64>,

    /// Allow pushing to the repository's default branch (refused otherwise)
    #[arg(long)]
    pub allow_default_branch: bool,

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
