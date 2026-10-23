//! CLI surface for the `vivi` binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::boot::PumpView;

mod board_command;
mod config_command;
mod doctor_command;
mod flow_command;
mod mailspace_command;
mod mcp_command;
mod monitor_command;
mod pool_command;
mod project_command;
mod proxy_command;
mod pump_command;
mod report_command;
mod role_command;
mod skills_command;
mod sync_command;
mod update_command;

pub use board_command::BoardCommand;
pub use config_command::ConfigCommand;
pub use doctor_command::DoctorCommand;
pub use flow_command::FlowCommand;
pub use mailspace_command::{
    AbsorbCommand, CycleCommand, DependCommand, GoalCommand, GraphActivateCommand,
    GraphApplyCommand, GraphAuditCommand, GraphBlockedCommand, GraphCommand, GraphCompleteCommand,
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
pub use mcp_command::{McpCommand, McpSubcommand};
pub use monitor_command::MonitorCommand;
pub use pool_command::{
    PoolAddCommand, PoolBandsCommand, PoolCleanCommand, PoolConfigSetCommand, PoolCreateCommand,
    PoolDestroyCommand, PoolDispatchCommand, PoolDoctorCommand, PoolListCommand,
    PoolReconcileCommand, PoolRemoveCommand, PoolResetCommand, PoolScaleCommand, PoolShowCommand,
    PoolStatusCommand, PoolSubcommand, PoolUnsetCommand, PoolWaitCommand,
};
pub use project_command::{ProjectCommand, ProjectCreateCommand};
pub use proxy_command::{
    ProxyCheckCommand, ProxyConfigArg, ProxyLogCommand, ProxyRunCommand, ProxySubcommand,
};
pub use pump_command::{
    PumpControlCommand, PumpDoneCommand, PumpHealthCommand, PumpHistoryCommand, PumpListCommand,
    PumpListStatus, PumpLogCommand, PumpOnceCommand, PumpPriorityClearCommand, PumpPriorityCommand,
    PumpPrioritySetCommand, PumpReopenCommand, PumpReplanCommand, PumpRequeueCommand,
    PumpRunCommand, PumpRungsCommand, PumpRungsSetCommand, PumpRungsShowCommand, PumpSendCommand,
    PumpServiceAddCommand, PumpServiceCommand, PumpServiceProjectCommand, PumpShowCommand,
    PumpSubcommand, PumpTargetAddCommand, PumpTargetCommand, PumpTargetListCommand,
    PumpTargetRemoveCommand, PumpWithdrawCommand,
};
pub use report_command::{
    ReportBy, ReportCommand, ReportCommon, ReportFormat, ReportRecordsCommand, ReportRecordsFormat,
    ReportSubcommand, ReportViewCommand,
};
pub use role_command::{RoleCharterCommand, RoleCommand};
pub use skills_command::SkillsCommand;
pub use sync_command::{ApiKeyCommand, SyncCommand, SyncSubcommand};
pub use update_command::{UninstallCommand, UpdateCommand, VersionsCommand};

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

    /// Give one read-only project health verdict and ranked findings
    Doctor(DoctorCommand),

    /// Show the whole project frame in one read: seats, loops, unabsorbed mail,
    /// handles with verdicts, goals with register tallies, and the backlog
    /// sliced for dispatch. Read-only and stateless, so it serves a cold boot
    /// and a post-compaction warm boot alike.
    Boot {
        /// Project root that owns .vivi/ (also accepted globally: vivi --project <ROOT> boot)
        #[arg(long)]
        project: Option<PathBuf>,
        /// Role to gate the pump snapshot for (otherwise inferred from a local slot)
        #[arg(long)]
        role: Option<String>,
        /// Pump snapshot depth: full, own (this role's units), or none (counts only)
        #[arg(long, value_enum)]
        pump: Option<PumpView>,
        /// Print the boot digest as JSON
        #[arg(long)]
        json: bool,
    },

    /// Push local letters to a project server, then pull what other members pushed
    Sync(SyncCommand),

    /// Bridge MCP over stdio to a project server's MCP endpoint (a pass-through; needs no mailspace); `tools` and `call` are one-shot clients
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

    /// Deliver an integration branch to a merge role and record its receipt
    Delivery {
        #[command(subcommand)]
        command: DeliveryCommand,
    },

    /// Send project-local wants; the needs that serve a want decide its state
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

    /// Create projects on a host: git repository, mailspace and registration
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },

    /// Run the pump: the flows that plan, slice, build, verify and merge each need with an open pump record
    Pump {
        #[command(subcommand)]
        command: PumpSubcommand,
    },

    /// Watch running lanes across every registered mailspace (read-only)
    Monitor(MonitorCommand),

    /// Export fixed project timing reports from synced attempt records
    Report(ReportCommand),

    /// Local loopback front door for model providers
    Proxy {
        #[command(subcommand)]
        command: ProxySubcommand,
    },

    /// Show, validate or migrate role → pool routing
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
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

    /// List and show the shipped flows, and a need's flow runs (read-only)
    Flow {
        #[command(subcommand)]
        command: FlowCommand,
    },

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

    /// Install or switch a public release binary
    Update(UpdateCommand),

    /// List versioned installs next to the running binary
    Versions(VersionsCommand),

    /// Remove versioned installs (not `proxy uninstall`, the OS service)
    Uninstall(UninstallCommand),

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

#[derive(Debug, Subcommand)]
pub enum DeliveryCommand {
    /// Push a scoped Mind's branch and file its structured delivery need
    Send {
        /// Identity filing the delivery
        #[arg(long = "from")]
        from_identity: String,

        /// Merge role address, such as merge@<member-prefix>.local
        #[arg(long = "to")]
        to: String,

        /// Local integration branch (the remote ref is placed under mind/)
        #[arg(long)]
        branch: String,

        /// Tip commit the gate result describes
        #[arg(long)]
        tip: String,

        /// Base commit from which the integration branch was built
        #[arg(long)]
        base: String,

        /// Git remote to push to
        #[arg(long, default_value = "origin")]
        remote: String,

        /// Gate command run against the tip
        #[arg(long)]
        gate: String,

        /// Result of that gate on the tip
        #[arg(long)]
        gate_result: String,

        /// Need closed by this delivery; repeatable
        #[arg(long = "closes")]
        closes: Vec<String>,

        /// Blocked need in HANDLE=REASON form; repeatable
        #[arg(long = "blocked")]
        blocked: Vec<String>,

        /// Newly filed work handle; repeatable
        #[arg(long = "filed")]
        filed: Vec<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Close a delivered branch after merge, recording the merge commit
    Close {
        /// Delivery need handle or unambiguous prefix
        handle: String,

        /// Merge identity accepting the delivery
        #[arg(long = "for")]
        for_identity: String,

        /// Commit that landed the delivered branch on main
        #[arg(long)]
        merge_commit: String,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
