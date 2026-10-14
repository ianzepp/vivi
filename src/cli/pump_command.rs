//! `vivi pump`: the pump as a state machine.

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum PumpSubcommand {
    /// Run the pump until the STOP file appears
    Run(PumpRunCommand),
    /// Print the pump's status file
    Status(PumpStatusCommand),
    /// Run one pump tick and exit
    Once(PumpOnceCommand),
}

#[derive(Debug, Args)]
pub struct PumpRunCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpStatusCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpOnceCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}
