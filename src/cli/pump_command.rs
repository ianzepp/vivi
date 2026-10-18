//! `vivi pump`: the pump as a state machine.

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum PumpSubcommand {
    /// Run the pump until its stored control state stops it
    Run(PumpRunCommand),
    /// Print the pump's status file
    Status(PumpStatusCommand),
    /// Print the pump's log (last lines; filter by lane, unit or text; follow)
    Log(PumpLogCommand),
    /// Run one pump tick and exit
    Once(PumpOnceCommand),
    /// Write the live pump.db as the state.json shape
    Export(PumpExportCommand),
    /// List complete but open needs, or close one
    Closeout(PumpCloseoutCommand),
    /// Pause processing while keeping the pump available
    Pause(PumpControlCommand),
    /// Resume a paused pump
    Resume(PumpControlCommand),
    /// Stop after the current tick
    Stop(PumpControlCommand),
    /// Enable a stopped pump; the service starts it when registered
    Start(PumpControlCommand),
    /// Set, clear, or list per-need dispatch priority
    Priority {
        #[command(subcommand)]
        command: PumpPriorityCommand,
    },
    /// Supervise the pumps registered in the user's Vivi service config
    Service {
        #[command(subcommand)]
        command: PumpServiceCommand,
    },
}

#[derive(Debug, Args)]
pub struct PumpControlCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum PumpPriorityCommand {
    /// Set a need's priority level; higher is more urgent, 0 removes it
    Set(PumpPrioritySetCommand),
    /// Remove a need's priority
    Clear(PumpPriorityClearCommand),
    /// List prioritized needs, highest level first
    List(PumpControlCommand),
}

#[derive(Debug, Args)]
pub struct PumpPrioritySetCommand {
    /// Need handle
    pub need: String,
    /// Priority level (integer, 0 or higher; higher is more urgent)
    pub level: i64,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpPriorityClearCommand {
    /// Need handle
    pub need: String,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum PumpServiceCommand {
    /// Add a project to the user-level service registry
    Add(PumpServiceProjectCommand),
    /// Remove a project from the user-level service registry
    Remove(PumpServiceProjectCommand),
    /// List registered projects
    List,
    /// Install and start the current Vivi binary as a user OS service
    Install,
    /// Stop and remove the user OS service, retaining registered projects
    Uninstall,
    /// Show OS service-manager status
    Status,
    /// Run the foreground supervisor (for an OS service manager)
    Run,
}

#[derive(Debug, Args)]
pub struct PumpServiceProjectCommand {
    /// Project root that owns .vivi/
    pub project: PathBuf,
}

#[derive(Debug, Args)]
pub struct PumpRunCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// Internal readiness handshake used by the pump service
    #[arg(long, hide = true)]
    pub service_child: bool,
}

#[derive(Debug, Args)]
pub struct PumpStatusCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpLogCommand {
    /// Number of lines to print, counted after filtering (0 with -f: only new lines)
    #[arg(short = 'n', long = "lines", default_value_t = 50)]
    pub lines: usize,
    /// Keep printing lines as they are appended
    #[arg(short = 'f', long)]
    pub follow: bool,
    /// Only merge-lane lines (`lane <L>:`); `2` means `default/2`
    #[arg(long, value_name = "LANE")]
    pub lane: Option<String>,
    /// Only lines naming this unit or need handle as a whole token
    #[arg(long, value_name = "HANDLE")]
    pub unit: Option<String>,
    /// Only lines containing this text (case-sensitive)
    #[arg(long, value_name = "TEXT")]
    pub grep: Option<String>,
    /// Read exceptions.txt instead of log.txt
    #[arg(long)]
    pub exceptions: bool,
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
