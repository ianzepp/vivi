use std::path::PathBuf;

use clap::{ArgGroup, Parser, Subcommand};

mod graph_command;
mod rule_command;
mod work_command;
pub use graph_command::{
    GraphActivateCommand, GraphApplyCommand, GraphAuditCommand, GraphBlockedCommand, GraphCommand,
    GraphCompleteCommand, GraphConnectCommand, GraphEdgeAddCommand, GraphEdgeCommand,
    GraphExportCommand, GraphImportCommand, GraphNodeAddCommand, GraphNodeCommand,
    GraphReadyCommand, GraphShowCommand,
};
pub use rule_command::{RuleCommand, RuleRetireCommand, RuleSaveCommand, RuleSupersedeCommand};
pub use work_command::{
    AbsorbCommand, DependCommand, GoalCommand, MailDumpCommand, MarkCommand, MemoCommand,
    NeedCommand, TaskDumpCommand, TaskDumpStatusArg, TaskStatus, WantCommand, WantStatus,
};

#[derive(Debug, Subcommand)]
pub enum MailspaceCommand {
    /// Explicitly initialize .vivi/ in a project root
    Init {
        /// Project root to initialize
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show mailspace root, store, identities, and waiting work
    Status {
        /// Project root to inspect
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show or set the mailspace description
    Description {
        /// Project root to inspect
        #[arg(long)]
        project: Option<PathBuf>,

        /// Set a new description; omit to show current
        #[arg(long)]
        set: Option<String>,
    },

    /// Show or set the historical archive git repository
    #[command(group(
        ArgGroup::new("archive_action")
            .args(["set", "clear"])
    ))]
    Archive {
        /// Project root to inspect
        #[arg(long)]
        project: Option<PathBuf>,

        /// Path to a dedicated git repo that receives absorbed records
        #[arg(long)]
        set: Option<String>,

        /// Clear the configured archive path
        #[arg(long)]
        clear: bool,

        #[command(subcommand)]
        command: Option<MailspaceArchiveCommand>,
    },

    /// Wait for project-local mailspace events; this is not IMAP watch
    Watch(Box<MailspaceWatchCommand>),

    /// Import another Vivi mailspace into this one
    Import(MailspaceImportCommand),

    /// Merge another Vivi mailspace into this one
    #[command(hide = true)]
    Merge(MailspaceImportCommand),

    /// Manage local identities in the explicit roster
    Identity {
        #[command(subcommand)]
        command: MailspaceIdentityCommand,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum MailspaceArchiveCommand {
    /// Write every absorbed record into the configured archive repo
    Export {
        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output the export report as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Parser)]
pub struct MailspaceImportCommand {
    /// Project root to import into
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Source project root or .vivi directory to import from
    #[arg(long = "from")]
    pub from: PathBuf,

    /// Report what would be imported without writing anything
    #[arg(long)]
    pub dry_run: bool,

    /// Output the import report as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum MailspaceIdentityCommand {
    /// Add a thin local identity (no role metadata). For a full role seat in
    /// one step with kind/labels/harness, prefer `vivi role add`.
    Add {
        /// Identity name
        identity: String,

        /// Project root to update
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// List local identities
    List {
        /// Project root to inspect
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Rename a local identity, keeping the old name as an alias so
    /// historical mail still resolves
    Rename {
        /// Current identity name
        old: String,

        /// New identity name
        new: String,

        /// Project root to update
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum CycleCommand {
    /// Collect cycle intake for an identity
    Intake {
        /// Identity whose cycle should be inspected
        #[arg(long = "for")]
        for_identity: String,

        /// Cursor file containing the last consumed mailspace event id
        #[arg(long)]
        cursor_file: Option<PathBuf>,

        /// Write the updated cursor after producing output
        #[arg(long)]
        write_cursor: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum MailCommand {
    /// Deliver local mail inside the current project mailspace only
    Send(LocalSendCommand),

    /// Wait for mail events in the project-local mailspace
    Watch(Box<KindWatchCommand>),

    /// Deliver an explicit .eml into local identities in the current project mailspace
    Deliver {
        /// Path to RFC 5322 .eml file
        path: PathBuf,

        /// Target local folder role
        #[arg(long, default_value = "inbox")]
        folder: String,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// List local mail for an identity or header filter
    List(MailListCommand),

    /// Show one or more local mail messages by handle
    Show {
        /// Local mail handle or unambiguous prefix
        #[arg(required = true)]
        handles: Vec<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show the project-local conversation containing a handle
    Thread(MailThreadCommand),

    /// Seal local mail. Once absorbed, it can no longer be changed.
    Absorb(work_command::AbsorbCommand),

    /// Send a project-local reply to any mailspace kind
    Reply(MailReplyCommand),

    /// Dump local mailspace messages for audit and board review
    Dump(MailDumpCommand),
}

#[derive(Debug, Clone, Parser)]
#[command(group(
    ArgGroup::new("list_scope")
        .required(true)
        .multiple(true)
        .args(["for_identity", "from", "to"])
))]
pub struct MailListCommand {
    /// Identity whose mailbox should be listed
    #[arg(long = "for")]
    pub for_identity: Option<String>,

    /// Sender identity or address filter
    #[arg(long)]
    pub from: Option<String>,

    /// Recipient identity or address filter (To or Cc)
    #[arg(long)]
    pub to: Option<String>,

    /// Folder role to list
    #[arg(long, default_value = "inbox")]
    pub folder: String,

    /// Absorb status filter
    #[arg(long, default_value = "all")]
    pub status: MailAbsorbStatus,

    /// Absorbing identity filter
    #[arg(long = "absorbed-by")]
    pub absorbed_by: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum MailAbsorbStatus {
    All,
    Absorbed,
    Unabsorbed,
}

#[derive(Debug, Clone, Parser)]
#[command(group(
    ArgGroup::new("body_input")
        .required(true)
        .args(["body", "body_file"])
))]
pub struct LocalSendCommand {
    /// Local sender identity or address
    #[arg(long)]
    pub from: String,

    /// Local To recipient; may be passed multiple times
    #[arg(long)]
    pub to: Vec<String>,

    /// Local Cc recipient; may be passed multiple times
    #[arg(long)]
    pub cc: Vec<String>,

    /// Rejected for local delivery in v1
    #[arg(long)]
    pub bcc: Vec<String>,

    /// Message subject
    #[arg(long)]
    pub subject: String,

    /// Existing mailspace handle to make this message a captured reply
    #[arg(long)]
    pub reply_to: Option<String>,

    /// Message body, or @path to read body from a file
    #[arg(long)]
    pub body: Option<String>,

    /// Read message body from a file
    #[arg(long)]
    pub body_file: Option<PathBuf>,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct TaskSendCommand {
    #[command(flatten)]
    pub send: LocalSendCommand,

    /// Task handle this task depends on (repeatable)
    #[arg(long = "depends-on")]
    pub depends_on: Vec<String>,

    /// What must be true for the task to be done; builds the front matter (repeatable)
    #[arg(long = "done-when")]
    pub done_when: Vec<String>,

    /// The repo or checkout the task targets; builds the front matter
    #[arg(long)]
    pub target: Option<String>,

    /// A path the task may change; builds the front matter (repeatable)
    #[arg(long = "write-scope")]
    pub write_scope: Vec<String>,
}

impl TaskSendCommand {
    /// The front matter flags of this send.
    #[must_use]
    pub fn sugar(&self) -> crate::letter::sugar::Sugar {
        crate::letter::sugar::Sugar {
            done_when: self.done_when.clone(),
            target: self.target.clone(),
            write_scope: self.write_scope.clone(),
            depends_on: self.depends_on.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct NeedSendCommand {
    #[command(flatten)]
    pub send: LocalSendCommand,

    /// Need or want handle this need depends on (repeatable)
    #[arg(long = "depends-on")]
    pub depends_on: Vec<String>,

    /// Mark slug to write on the new need, as the sending identity (repeatable)
    #[arg(long = "mark")]
    pub marks: Vec<String>,

    /// What must be true for the need to be done; builds the front matter (repeatable)
    #[arg(long = "done-when")]
    pub done_when: Vec<String>,

    /// The repo or checkout the need targets; builds the front matter
    #[arg(long)]
    pub target: Option<String>,

    /// A path the need may change; builds the front matter (repeatable)
    #[arg(long = "write-scope")]
    pub write_scope: Vec<String>,

    /// The want this need serves; builds the front matter
    #[arg(long)]
    pub serves: Option<String>,

    /// The want this research need explores; builds the front matter
    #[arg(long)]
    pub explores: Option<String>,
}

impl NeedSendCommand {
    /// The front matter flags of this send.
    #[must_use]
    pub fn sugar(&self) -> crate::letter::sugar::Sugar {
        crate::letter::sugar::Sugar {
            done_when: self.done_when.clone(),
            target: self.target.clone(),
            write_scope: self.write_scope.clone(),
            serves: self.serves.clone(),
            explores: self.explores.clone(),
            depends_on: self.depends_on.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Parser)]
pub struct WantSendCommand {
    #[command(flatten)]
    pub send: LocalSendCommand,

    /// Need or want handle this want depends on (repeatable)
    #[arg(long = "depends-on")]
    pub depends_on: Vec<String>,

    /// The question or goal the want explores; builds the front matter
    #[arg(long)]
    pub question: Option<String>,
}

impl WantSendCommand {
    /// The front matter flags of this send.
    #[must_use]
    pub fn sugar(&self) -> crate::letter::sugar::Sugar {
        crate::letter::sugar::Sugar {
            question: self.question.clone(),
            depends_on: self.depends_on.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(group(
    ArgGroup::new("reply_body")
        .required(true)
        .args(["body", "body_file"])
))]
pub struct MailReplyCommand {
    /// Existing mailspace handle to reply to
    pub handle: String,

    /// Local sender identity
    #[arg(long)]
    pub from: String,

    /// Optional explicit To recipient; defaults to the parent sender
    #[arg(long)]
    pub to: Vec<String>,

    /// Optional Cc recipient
    #[arg(long)]
    pub cc: Vec<String>,

    /// Reply subject; defaults to a single Re: prefix
    #[arg(long)]
    pub subject: Option<String>,

    /// Reply body, or @path to read body from a file
    #[arg(long)]
    pub body: Option<String>,

    /// Read reply body from a file
    #[arg(long)]
    pub body_file: Option<PathBuf>,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct MailThreadCommand {
    /// Any local mailspace handle or unambiguous prefix
    pub handle: String,

    /// Include best-effort historical links in this read view
    #[arg(long)]
    pub infer: bool,

    /// Maximum number of thread messages to include
    #[arg(long, default_value = "50")]
    pub limit: usize,

    /// Maximum ancestor/descendant depth to walk
    #[arg(long, default_value = "50")]
    pub max_depth: usize,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct TraceCommand {
    /// Any local mailspace handle or unambiguous prefix
    pub handle: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Maximum ancestor/descendant depth to walk
    #[arg(long, default_value = "5")]
    pub max_depth: usize,

    /// Maximum number of nodes to include
    #[arg(long, default_value = "100")]
    pub limit: usize,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Shared watch filters. `--kinds` belongs only on `vivi mailspace watch`.
#[derive(Debug, Clone, Parser)]
#[allow(clippy::struct_excessive_bools)]
pub struct WatchCommon {
    /// Identity whose local events should wake the watcher
    #[arg(long = "for")]
    pub for_identity: String,

    /// Comma-separated raw event types
    #[arg(long, default_value = "delivered,moved")]
    pub events: String,

    /// Optional comma-separated derived destination statuses
    #[arg(long)]
    pub statuses: Option<String>,

    /// Optional sender identity filter
    #[arg(long)]
    pub match_from: Option<String>,

    /// Optional case-sensitive subject prefix filter
    #[arg(long)]
    pub match_subject_prefix: Option<String>,

    /// Optional handle whose message must change
    #[arg(long)]
    pub handle: Option<String>,

    /// Number of matches before exit; zero follows until interrupted
    #[arg(long, default_value_t = 1)]
    pub until_count: usize,

    /// Exit nonzero if no match arrives in this duration
    #[arg(long)]
    pub timeout: Option<String>,

    /// Scan once and exit without blocking
    #[arg(long)]
    pub once: bool,

    /// Initial time lower bound, used only without a cursor file
    #[arg(long)]
    pub since: Option<String>,

    /// Caller-owned event-id cursor file
    #[arg(long, conflicts_with = "watermark_file")]
    pub cursor_file: Option<PathBuf>,

    /// Alias for the caller-owned event-id cursor file
    #[arg(long, conflicts_with = "cursor_file")]
    pub watermark_file: Option<PathBuf>,

    /// Write the cursor after a successful scan/match
    #[arg(long)]
    pub write_cursor: bool,

    /// Alias for --write-cursor
    #[arg(long)]
    pub write_watermark: bool,

    /// Poll interval, such as 250ms, 2s, or a bare number of seconds
    #[arg(long, default_value = "250ms")]
    pub poll_interval: String,

    /// Output one JSON object per matching event
    #[arg(long)]
    pub json: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Kind-specific watch (`mail watch`, `task watch`, …). No `--kinds`.
#[derive(Debug, Clone, Parser)]
pub struct KindWatchCommand {
    #[command(flatten)]
    pub common: WatchCommon,
}

/// Cross-kind watch. `--kinds` is valid only here.
#[derive(Debug, Clone, Parser)]
pub struct MailspaceWatchCommand {
    #[command(flatten)]
    pub common: WatchCommon,

    /// Comma-separated kinds; defaults to mail,task,need
    #[arg(long, default_value = "mail,task,need")]
    pub kinds: String,
}

#[derive(Debug, Clone, Parser)]
#[command(group(
    ArgGroup::new("task_from_body")
        .required(true)
        .args(["body", "body_file"])
))]
pub struct TaskFromCommand {
    /// Source mailspace handle or unambiguous prefix
    pub handle: String,

    /// Identity assigning the task
    #[arg(long = "for")]
    pub for_identity: String,

    /// Local To recipient; may be passed multiple times
    #[arg(long)]
    pub to: Vec<String>,

    /// Local Cc recipient; may be passed multiple times
    #[arg(long)]
    pub cc: Vec<String>,

    /// Message subject
    #[arg(long)]
    pub subject: String,

    /// Message body, or @path to read body from a file
    #[arg(long)]
    pub body: Option<String>,

    /// Read message body from a file
    #[arg(long)]
    pub body_file: Option<PathBuf>,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Send a task message into the recipient's Tasks folder
    Send(TaskSendCommand),

    /// Create a task from an existing source handle
    From(TaskFromCommand),

    /// Wait for task events in the project-local mailspace
    Watch(Box<KindWatchCommand>),

    /// List tasks for an identity or header filter
    #[command(group(
        ArgGroup::new("list_scope")
            .required(true)
            .multiple(true)
            .args(["for_identity", "from", "to", "held"])
    ))]
    List {
        /// Identity whose tasks should be listed
        #[arg(long = "for")]
        for_identity: Option<String>,

        /// List tasks held under any other mailspace's address
        #[arg(long)]
        held: bool,

        /// Task creator identity or address filter
        #[arg(long)]
        from: Option<String>,

        /// Task owner identity or address filter
        #[arg(long)]
        to: Option<String>,

        /// Task folder status
        #[arg(long, default_value = "open")]
        status: TaskStatus,

        /// List only tasks blocked by an unfinished dependency
        #[arg(long)]
        blocked: bool,

        /// List tasks that depend on the given handle
        #[arg(long)]
        blocking: Option<String>,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show a task root message
    Show {
        /// Task handle or unambiguous prefix
        handle: String,

        /// Include thread context as JSON
        #[arg(long)]
        json: bool,

        /// Print only the newest message of the thread
        #[arg(long)]
        latest: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Dump local task messages for audit and board review
    Dump(TaskDumpCommand),

    /// Seal a task. Once absorbed, it can no longer be changed.
    Absorb(work_command::AbsorbCommand),

    /// Add or remove prerequisites of a task after it was sent. The change
    /// travels to every member; the newest change per dependency wins,
    /// including over a dependency named at send.
    Depend(work_command::DependCommand),

    /// Move a task from Tasks to Done
    Done {
        /// Task handle or unambiguous prefix
        handle: String,

        /// Identity completing the task
        #[arg(long = "for")]
        for_identity: String,

        /// Optional completion note recorded in the mailspace event ledger
        #[arg(long)]
        note: Option<String>,

        /// Optional close verdict (e.g. `clean_pass`, residual, `block_ship`)
        #[arg(long)]
        verdict: Option<String>,

        /// Repository name; repeatable and paired with --tip
        #[arg(long)]
        repo: Vec<String>,

        /// Tip commit SHA; repeatable and paired with --repo
        #[arg(long)]
        tip: Vec<String>,

        /// Integration branch holding the completed task; paired with --commit
        #[arg(long, requires = "commit")]
        branch: Option<String>,

        /// Commit on the integration branch holding the completed task; paired with --branch
        #[arg(long, requires = "branch")]
        commit: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Move a task from Done back to Tasks
    Reopen {
        /// Task handle or unambiguous prefix
        handle: String,

        /// Identity reopening the task
        #[arg(long = "for")]
        for_identity: String,

        /// Optional reopen note recorded in the mailspace event ledger
        #[arg(long)]
        note: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },
}
