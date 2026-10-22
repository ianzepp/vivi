//! `vivi pump`: the pump as a state machine.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Subcommand)]
pub enum PumpSubcommand {
    /// Run the pump until its stored control state stops it
    Run(PumpRunCommand),
    /// Print the pump's status file
    Status(PumpStatusCommand),
    /// Judge the pump read-only: ok, degraded, stalled or unknown, with reasons
    Health(PumpHealthCommand),
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
    /// Show or set the gate command (`[pump.gate] command`)
    Gate {
        #[command(subcommand)]
        command: PumpGateCommand,
    },
    /// Requeue one or more units: posts an action on the need's open pump record, applied at the pump's next tick
    Requeue(PumpRequeueCommand),
    /// Withdraw every unfinished unit of one or more needs and close their pump record `withdrawn`; applied at the pump's next tick
    Withdraw(PumpWithdrawCommand),
    /// Supersede a need's plan: closes its pump record `superseded`, opens a fresh one and plans it again
    Replan(PumpReplanCommand),
    /// File a pump record: one dispatch of one need, addressed to the pump
    Send(PumpSendCommand),
    /// Open a pump record for every need the pump has in flight and no record for (the vivi 19 upgrade step)
    Adopt(PumpAdoptCommand),
    /// List pump records (default: open ones)
    List(PumpListCommand),
    /// Show one pump record
    Show(PumpShowCommand),
    /// Close a pump record, with an optional outcome note
    Done(PumpDoneCommand),
    /// Reopen a closed pump record, unless its need has another open record
    Reopen(PumpReopenCommand),
    /// All pump records of one need, oldest first, with state
    History(PumpHistoryCommand),
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
pub enum PumpGateCommand {
    /// Print the current gate command
    Show(PumpControlCommand),
    /// Set the gate command in this project's mailspace.toml
    Set(PumpGateSetCommand),
}

#[derive(Debug, Args)]
pub struct PumpGateSetCommand {
    /// Shell command the gate runs; must not be blank
    pub command: String,
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
    /// Reason posted with the action and recorded in the pump log (default `operator requeue`)
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
    /// Print what would change without posting an action
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

/// Supersede a need's plan and plan it again from the whole thread. Posts a
/// replan action on the need's open pump record; the running pump applies it
/// at its next tick: the plan moves to `plans/superseded/`, the need record
/// and its replies are forgotten, pending, held, stuck and needs-replan units
/// are withdrawn and never dispatched, while merged units stay merged and live
/// work finishes on its own. The open record is closed `superseded` and a
/// fresh one opened. Refuses while any unit is inflight, delivered, merging,
/// quarantined or splitting, or while a planner runs for the need, unless
/// `--force`, and when the need has no open pump record.
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
    /// Add a project to the host registry, or change its pump/serve flags
    Add(PumpServiceAddCommand),
    /// Remove a project from the host registry
    Remove(PumpServiceProjectCommand),
    /// List registered projects with their pump/serve flags
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

/// A new registration defaults to pump on, serve off; on an existing entry
/// only the flags given here change.
#[derive(Debug, Args)]
pub struct PumpServiceAddCommand {
    /// Project root that owns .vivi/
    pub project: PathBuf,
    /// Run a pump for this project under the service supervisor
    #[arg(long, conflicts_with = "no_pump")]
    pub pump: bool,
    /// Do not run a pump for this project (for example, serve-only)
    #[arg(long)]
    pub no_pump: bool,
    /// Let a server host this project
    #[arg(long, conflicts_with = "no_serve")]
    pub serve: bool,
    /// Stop a server hosting this project
    #[arg(long)]
    pub no_serve: bool,
}

impl PumpServiceAddCommand {
    /// The requested flag changes; an absent flag pair means no change.
    #[must_use]
    pub fn flag_change(&self) -> (Option<bool>, Option<bool>) {
        (
            tri_state(self.pump, self.no_pump),
            tri_state(self.serve, self.no_serve),
        )
    }
}

fn tri_state(on: bool, off: bool) -> Option<bool> {
    match (on, off) {
        (true, _) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    }
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
pub struct PumpHealthCommand {
    /// Output the full answer as JSON
    #[arg(long)]
    pub json: bool,
    /// Report what changed after this epoch second (a previous `as_of`)
    #[arg(long, value_name = "EPOCH")]
    pub since: Option<u64>,
    /// Seconds without a tick before a running pump counts as stalled
    #[arg(long, value_name = "SECS")]
    pub stale_after: Option<u64>,
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

/// Close needs inside the pump without touching the mail board. Posts a
/// withdraw action on each need's open pump record; the running pump applies
/// it at its next tick: every unfinished unit of the need (stuck ones
/// included) is withdrawn, the need joins the closed set so nothing re-plans
/// or dispatches it, its need-audit, need-merge and landing steps skip it, and
/// its pump record is closed `withdrawn`. Refused for a need with no open pump
/// record.
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

/// Open a pump record for every need the pump has planned (or is planning)
/// and not finished, unless it already has an open record. Idempotent. Run it
/// once after upgrading to vivi 19, so work that was in flight keeps replying.
#[derive(Debug, Args)]
pub struct PumpAdoptCommand {
    /// Print the needs that would be adopted and open nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// File one open pump record for one need. Refused while the need already has
/// an open record (the error names it).
#[derive(Debug, Args)]
pub struct PumpSendCommand {
    /// Need handle this dispatch covers
    #[arg(long, value_name = "HANDLE")]
    pub need: String,
    /// Sender identity (default `[pump] from`)
    #[arg(long, value_name = "ID")]
    pub from: Option<String>,
    /// Record body, `-` for stdin, or @path to read it from a file
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Which pump records `pump list` shows.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PumpListStatus {
    Open,
    Done,
    All,
}

#[derive(Debug, Args)]
pub struct PumpListCommand {
    /// Only records of this need
    #[arg(long, value_name = "HANDLE")]
    pub need: Option<String>,
    /// Which records to show
    #[arg(long, value_enum, default_value = "open")]
    pub status: PumpListStatus,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpShowCommand {
    /// Pump record handle
    pub handle: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpDoneCommand {
    /// Pump record handle
    pub handle: String,
    /// Outcome, such as landed, gave-up, withdrawn or superseded
    #[arg(long, value_name = "TEXT")]
    pub note: Option<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpReopenCommand {
    /// Pump record handle
    pub handle: String,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpHistoryCommand {
    /// Need handle
    pub need: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}
