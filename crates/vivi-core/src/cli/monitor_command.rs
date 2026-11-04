//! Arguments for the machine-wide, read-only lane monitor.

use std::path::PathBuf;

use clap::Args;

#[derive(Debug, Args)]
pub struct MonitorCommand {
    /// Extra project roots for this session (registered pump projects are included)
    #[arg(long = "project", value_name = "ROOT")]
    pub projects: Vec<PathBuf>,
    /// Take one snapshot and exit without opening a terminal
    #[arg(long)]
    pub once: bool,
    /// With --once, print the snapshot rows as JSON
    #[arg(long, requires = "once")]
    pub json: bool,
}
