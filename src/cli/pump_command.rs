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
    /// Write the live pump.db as the state.json shape
    Export(PumpExportCommand),
    /// List complete but open needs, or close one
    Closeout(PumpCloseoutCommand),
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

#[derive(Debug, Args)]
pub struct PumpCloseoutCommand {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Close this complete need (handle or unambiguous prefix)
    #[arg(long, value_name = "HANDLE")]
    pub close: Option<String>,
    /// Identity performing the close (required with --close)
    #[arg(long = "for")]
    pub for_identity: Option<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpExportCommand {
    /// Output path for the state.json-shaped export
    #[arg(long, value_name = "PATH")]
    pub json: PathBuf,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}
