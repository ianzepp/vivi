//! `vivi pool`: remote lane pool.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum PoolSubcommand {
    /// Create and seed the missing slots up to the pool size
    Create(PoolCreateCommand),
    /// List the pool's slots from the provider
    Status(PoolStatusCommand),
    /// Destroy one slot or all of them
    Destroy(PoolDestroyCommand),
    /// Hand a task to a free slot and return at once
    Dispatch(PoolDispatchCommand),
    /// Block until a dispatched task finishes, then collect it
    Wait(PoolWaitCommand),
    /// Collect finished dispatches, stop overdue ones, free their slots
    Reconcile(PoolReconcileCommand),
}

#[derive(Debug, Args)]
pub struct PoolDispatchCommand {
    /// Task handle; its body is the brief
    pub task: String,

    /// Model slug; must be one of `[pool.harness].models`
    #[arg(long)]
    pub model: String,

    /// Commit or branch to start from (default: a `from:` line in the task body)
    #[arg(long)]
    pub from: Option<String>,

    /// Branch to push to (default: a `branch:` line in the task body)
    #[arg(long)]
    pub branch: Option<String>,

    /// Hard time limit for the agent, in seconds (default 1800)
    #[arg(long)]
    pub budget: Option<u64>,

    /// Allow pushing to the repository's default branch (refused otherwise)
    #[arg(long)]
    pub allow_default_branch: bool,

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

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolReconcileCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolCreateCommand {
    /// Pool size to reach (default and maximum: the configured `slots`)
    #[arg(long)]
    pub slots: Option<u32>,

    /// Commit, branch, or tag to seed at (default: the repo's default branch tip)
    #[arg(long)]
    pub base: Option<String>,

    /// Give up waiting on a slot's seed after this many seconds
    #[arg(long, default_value_t = 900)]
    pub timeout: u64,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PoolStatusCommand {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

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

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}
