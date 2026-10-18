//! CLI surface for the `vivi` binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod board_command;
mod mailspace_command;
mod mcp_command;
mod pool_command;
mod pump_command;
mod role_command;
mod skills_command;
mod sync_command;

pub use board_command::BoardCommand;
pub use mailspace_command::{
    AbsorbCommand, CycleCommand, GoalCommand, GraphActivateCommand, GraphApplyCommand,
    GraphAuditCommand, GraphBlockedCommand, GraphCommand, GraphCompleteCommand,
    GraphConnectCommand, GraphEdgeAddCommand, GraphEdgeCommand, GraphExportCommand,
    GraphImportCommand, GraphNodeAddCommand, GraphNodeCommand, GraphReadyCommand, GraphShowCommand,
    KindWatchCommand, LocalSendCommand, MailAbsorbStatus, MailCommand, MailDumpCommand,
    MailListCommand, MailReplyCommand, MailThreadCommand, MailspaceArchiveCommand,
    MailspaceCommand, MailspaceIdentityCommand, MailspaceImportCommand, MailspaceWatchCommand,
    MarkCommand, MemoCommand, NeedCommand, NeedSendCommand, RuleCommand, RuleRetireCommand,
    RuleSaveCommand, RuleSupersedeCommand, TaskCommand, TaskDumpCommand, TaskDumpStatusArg,
    TaskFromCommand, TaskSendCommand, TaskStatus, TraceCommand, WantCommand, WantSendCommand,
    WantStatus, WatchCommon,
};
pub use mcp_command::McpCommand;
pub use pool_command::{
    PoolCreateCommand, PoolDestroyCommand, PoolDispatchCommand, PoolDoctorCommand,
    PoolReconcileCommand, PoolScaleCommand, PoolStatusCommand, PoolSubcommand, PoolWaitCommand,
};
pub use pump_command::{
    PumpCloseoutCommand, PumpExportCommand, PumpOnceCommand, PumpRunCommand, PumpServiceCommand,
    PumpServiceProjectCommand, PumpStatusCommand, PumpSubcommand,
};
pub use role_command::{RoleCharterCommand, RoleCommand};
pub use skills_command::SkillsCommand;
pub use sync_command::{ApiKeyCommand, SyncCommand, SyncSubcommand};

#[derive(Debug, Parser)]
#[command(
    name = "vivi",
    version,
    about = "Local-first project mailspace for LLM agents"
)]
pub struct Cli {
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Project root that owns .vivi/ (also accepted after the subcommand)
    #[arg(long, global = true)]
    pub project: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show project-local actionable work across tasks, needs, and wants
    Board(BoardCommand),

    /// Show the whole project frame in one read: seats, loops, unabsorbed mail,
    /// handles with verdicts, goals with register tallies, and the backlog
    /// sliced for dispatch. Read-only and stateless, so it serves a cold boot
    /// and a post-compaction warm boot alike.
    Boot {
        /// Project root that owns .vivi/ (also accepted globally: vivi --project <ROOT> boot)
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Push local letters to a project server, then pull what other members pushed
    Sync(SyncCommand),

    /// Bridge MCP over stdio to a project server's MCP endpoint (a pass-through; needs no mailspace)
    Mcp(McpCommand),

    /// Manage a project-local Vivi mailspace
    Mailspace {
        #[command(subcommand)]
        command: MailspaceCommand,
    },

    /// Send and inspect project-local mail with no external side effects
    Mail {
        #[command(subcommand)]
        command: MailCommand,
    },

    /// Send and complete project-local tasks as folder-based mail
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },

    /// Send and complete project-local needs as prioritized mail
    Need {
        #[command(subcommand)]
        command: NeedCommand,
    },

    /// Send and promote project-local wants for later prioritization
    Want {
        #[command(subcommand)]
        command: WantCommand,
    },

    /// Save and inspect project-local memos as durable role memory
    Memo {
        #[command(subcommand)]
        command: MemoCommand,
    },

    /// Save and inspect project rules shared by every seat
    Rule {
        #[command(subcommand)]
        command: RuleCommand,
    },

    /// Register goal document paths the Mind should keep monitoring
    Goal {
        #[command(subcommand)]
        command: GoalCommand,
    },

    /// Remote lane pool of preseeded machines
    Pool {
        #[command(subcommand)]
        command: PoolSubcommand,
    },

    /// Run the pump: dispatch, merge and gate the planned units of a backlog
    Pump {
        #[command(subcommand)]
        command: PumpSubcommand,
    },

    /// Manage first-class mailspace agent seats (roles)
    Role {
        #[command(subcommand)]
        command: RoleCommand,
    },

    /// Inspect project-local agent cycle intake
    Cycle {
        #[command(subcommand)]
        command: CycleCommand,
    },

    /// Trace the cross-role communication tree around a handle
    Trace(TraceCommand),

    /// Import and inspect executable work graphs
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },

    /// Show or install the agent skills embedded in this binary
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },

    /// Adjudicate the backlog graph into a dispatch/exception manifest
    Step {
        /// Settled item handle to adjudicate and complete (apply mode)
        #[arg(long)]
        apply: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}
