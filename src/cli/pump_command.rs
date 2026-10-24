//! `vivi pump`: the pump as a state machine.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Subcommand)]
pub enum PumpSubcommand {
    /// Run the pump until its stored control state stops it
    Run(PumpRunCommand),
    /// Judge the pump read-only: ok, degraded, stalled or unknown, with reasons
    Health(PumpHealthCommand),
    /// Print the pump's log (last lines; filter by handle or text; follow)
    Log(PumpLogCommand),
    /// Run one pump tick and exit
    Once(PumpOnceCommand),
    /// Pause processing while keeping the pump available
    Pause(PumpControlCommand),
    /// Resume a paused pump
    Resume(PumpControlCommand),
    /// Stop after the current tick
    Stop(PumpControlCommand),
    /// Enable a stopped pump; the service starts it when registered
    Start(PumpControlCommand),
    /// Archive the pump home and start fresh; refused while the pump runs
    Reset(PumpResetCommand),
    /// Set, clear, or list per-need dispatch priority
    Priority {
        #[command(subcommand)]
        command: PumpPriorityCommand,
    },
    /// Show or set the proof rung commands (`[pump.rungs]`)
    Rungs {
        #[command(subcommand)]
        command: PumpRungsCommand,
    },
    /// List, add or remove repository targets (`[pump.targets.<id>]`); writes are refused while the pump is running
    Target {
        #[command(subcommand)]
        command: PumpTargetCommand,
    },
    /// Requeue flow steps: posts a requeue on each step's need's open pump record; the pump queues the step's job again at its next tick
    Requeue(PumpRequeueCommand),
    /// Close one or more needs `withdrawn` and post a withdraw on each open pump record; the pump closes the record at its next tick
    Withdraw(PumpWithdrawCommand),
    /// Restart a need's live flow run at plan: posts a replan on the need's open pump record, applied at the pump's next tick
    Replan(PumpReplanCommand),
    /// Answer a flow step by hand: posts the word on the need's open pump record; the pump routes it at its next tick
    Answer {
        /// The step task handle (see `vivi step` or `vivi flow show <kind> --need <need>`)
        step: String,
        /// The answer word (one of the step's words, `failed` included)
        word: String,
        /// Free text recorded with the answer
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// Project root that owns .vivi/ (also accepted globally)
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// Unlock a need whose flow run is locked: posts the unlock on the need's open pump record; the pump restarts the run at its next tick. Default entry is the flow's first entry (plan for build, fix1 for repair); a reply on the need never releases a lock
    Unlock {
        /// Need handle
        need: String,
        /// Flow entry to restart at (default: the flow's first entry)
        #[arg(long, value_name = "ENTRY")]
        entry: Option<String>,
        /// Free text recorded with the unlock
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// Project root that owns .vivi/ (also accepted globally)
        #[arg(long)]
        project: Option<PathBuf>,
    },
    /// File a pump record: one dispatch of one need, addressed to the pump
    Send(PumpSendCommand),
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

#[derive(Debug, Args)]
pub struct PumpResetCommand {
    /// Output the full answer as JSON
    #[arg(long)]
    pub json: bool,
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
pub enum PumpRungsCommand {
    /// Print the four rung commands, the timeout and the pools
    Show(PumpRungsShowCommand),
    /// Set one rung command in this project's mailspace.toml
    Set(PumpRungsSetCommand),
}

#[derive(Debug, Args)]
pub struct PumpRungsShowCommand {
    /// Show the effective rungs of this target
    #[arg(long)]
    pub target: Option<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpRungsSetCommand {
    /// Rung name: task, need, main or release
    pub rung: String,
    /// Shell command the rung runs; must not be blank
    pub command: String,
    /// Write the target's own override instead of the workspace rung
    #[arg(long)]
    pub target: Option<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum PumpTargetCommand {
    /// List the configured targets
    List(PumpTargetListCommand),
    /// Add a target, with its rungs and merge settings
    Add(PumpTargetAddCommand),
    /// Remove a target
    Remove(PumpTargetRemoveCommand),
}

#[derive(Debug, Args)]
pub struct PumpTargetListCommand {
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpTargetAddCommand {
    /// Target id (the `[pump.targets.<id>]` key)
    pub id: String,
    /// Repository path, relative to the project root or absolute
    #[arg(long)]
    pub path: String,
    /// Remote URL used to clone or fetch the target
    #[arg(long)]
    pub remote_url: Option<String>,
    /// Remote name
    #[arg(long)]
    pub remote: Option<String>,
    /// Main branch name
    #[arg(long)]
    pub main: Option<String>,
    /// GitHub repository, as owner/name
    #[arg(long)]
    pub github_repo: Option<String>,
    /// Rung command for this target; repeatable (rung name: task, need, main or release)
    #[arg(long = "rung", value_name = "NAME=CMD")]
    pub rungs: Vec<String>,
    /// Merge setting; repeatable (format or remote_limit; the value is read as TOML)
    #[arg(long = "merge", value_name = "KEY=VALUE")]
    pub merge: Vec<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpTargetRemoveCommand {
    /// Target id to remove
    pub id: String,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PumpRequeueCommand {
    /// Step task handles to requeue (see `vivi flow show <kind> --need <need>`)
    #[arg(value_name = "STEP")]
    pub steps: Vec<String>,
    /// Reason posted with the action and recorded in the pump log (default `operator requeue`)
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
    /// Print what would change without posting an action
    #[arg(long)]
    pub dry_run: bool,
    /// Release this held pool job; the pump applies it at its next tick
    #[arg(long, value_name = "JOB", conflicts_with = "steps")]
    pub job: Option<String>,
    /// Project root that owns .vivi/ (also accepted globally)
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Restart a need's live flow run at its plan step. Posts a replan action on
/// the need's open pump record; the running pump applies it at its next tick.
/// Refused when the need has no open pump record.
#[derive(Debug, Args)]
pub struct PumpReplanCommand {
    /// Need handle to replan
    pub need: String,
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
    /// Only lines naming this handle as a whole token
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

/// Stop needs. Closes each need `withdrawn` (as the pump identity, so the
/// close syncs), then posts a withdraw action on its open pump record; the
/// running pump applies it at its next tick and closes the record
/// `withdrawn`. Refused for a need with no open pump record.
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
    /// Outcome, such as landed, withdrawn or superseded
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
