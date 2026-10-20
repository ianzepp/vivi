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
    /// Queue a requeue for one or more units; the pump applies it at its next tick
    Requeue(PumpRequeueCommand),
    /// Withdraw every unfinished unit of one or more needs and stop their pipeline; the pump applies it at its next tick
    Withdraw(PumpWithdrawCommand),
    /// Supersede a need's plan and queue a fresh planner run
    Replan(PumpReplanCommand),
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

#[derive(Debug, Args)]
pub struct PumpRequeueCommand {
    /// Unit handles to requeue
    #[arg(value_name = "UID")]
    pub units: Vec<String>,
    /// Set the attempt counter to N instead of keeping it
    #[arg(long, value_name = "N")]
    pub attempts: Option<u32>,
    /// Refund the attempts that failed for a transient reason
    #[arg(long)]
    pub refund_host_failures: bool,
    /// Reset the attempt counter to zero
    #[arg(long)]
    pub fresh: bool,
    /// Reason recorded in the pump log (default `operator requeue`)
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
    /// Print what would change without writing a request
    #[arg(long)]
    pub dry_run: bool,
    /// Requeue every unit stuck since this time (bulk form)
    #[arg(long, value_name = "TIME")]
    pub stuck_since: Option<String>,
    /// Restrict the requeue to this failure class (bulk form)
    #[arg(long, value_name = "CLASS")]
    pub why: Option<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Supersede a need's plan and queue a fresh planner run that reads the whole
/// thread. The running pump applies it at its next tick: the plan moves to
/// `plans/superseded/`, the need record and its replies are forgotten, pending,
/// held, stuck and needs-replan units are withdrawn and never dispatched,
/// while merged units stay merged and live work finishes on its own. Refuses
/// while any unit is inflight, delivered, merging, quarantined or splitting,
/// or while a planner runs for the need, unless `--force`.
#[derive(Debug, Args)]
pub struct PumpReplanCommand {
    /// Need handle to replan
    pub need: String,
    /// Replan even while units are inflight, delivered, merging, quarantined or splitting, or a planner runs
    #[arg(long)]
    pub force: bool,
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

/// Close needs inside the pump without touching the mail board. The running
/// pump applies the request at its next tick: every unfinished unit of the
/// need (stuck ones included) is withdrawn, the need joins the closed set so
/// nothing re-plans or dispatches it, and its need-audit, need-merge and
/// landing steps skip it. Meant for needs that have no mail thread (the
/// synthetic `pumpfix-*` repair needs of Vivi 15), but it works for any need.
#[derive(Debug, Args)]
pub struct PumpWithdrawCommand {
    /// Need handles to withdraw
    #[arg(value_name = "NEED", required = true)]
    pub needs: Vec<String>,
    /// Why the need is withdrawn; recorded in the pump log and exceptions
    #[arg(long, value_name = "TEXT")]
    pub reason: String,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}
