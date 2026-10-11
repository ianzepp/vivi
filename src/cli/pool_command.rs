//! `vivi pool`: remote lane pool (first iteration).

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
