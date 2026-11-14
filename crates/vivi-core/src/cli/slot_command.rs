//! `vivi slot`: the declared slots and the jobs they ran.

use std::path::PathBuf;

use clap::{Args, Subcommand};

/// Read the `slot_runs` index, which `vivi` keeps in step with the pool events.
#[derive(Debug, Subcommand)]
pub enum SlotSubcommand {
    /// List the declared slots, each busy (with its job) or free; reads records, contacts no host
    List(SlotListCommand),
    /// Show one slot and the job it is running now
    Show(SlotShowCommand),
    /// List the jobs a slot ran, newest first
    History(SlotHistoryCommand),
    /// Rebuild the slot index from the pool events (safe to repeat)
    Rebuild(SlotRebuildCommand),
}

#[derive(Debug, Args)]
pub struct SlotListCommand {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SlotShowCommand {
    /// Slot name (a `[[slot]]` name)
    pub slot: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SlotHistoryCommand {
    /// Slot name (a `[[slot]]` name)
    pub slot: String,

    /// Pool name (`[pools.<name>]`); without it, the slot's jobs in every configured pool
    #[arg(long)]
    pub pool: Option<String>,

    /// Most jobs to list
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SlotRebuildCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}
