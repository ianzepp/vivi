use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use super::MailAbsorbStatus;

/// Add or remove marks on a need or a want.
#[derive(Debug, Clone, Args)]
pub struct MarkCommand {
    /// Need or want handle or unambiguous prefix
    pub handle: String,

    /// Mark slug. Repeat to write one event per slug.
    #[arg(required = true, value_name = "SLUG")]
    pub slugs: Vec<String>,

    /// Identity that holds the item
    #[arg(long = "for")]
    pub for_identity: String,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Seal a mailspace record. Once absorbed, it can no longer be changed.
#[derive(Debug, Clone, Args)]
pub struct AbsorbCommand {
    /// Record handle or unambiguous prefix
    pub handle: String,

    /// Identity absorbing the record
    #[arg(long = "for")]
    pub for_identity: String,

    /// Optional disposition note recorded in the event ledger
    #[arg(long)]
    pub note: Option<String>,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

/// Save a memo into an identity's memos folder.
#[derive(Debug, Clone, Args)]
#[command(group(
    clap::ArgGroup::new("memo_body_input")
        .required(true)
        .args(["body", "body_file"])
))]
pub struct MemoSaveCommand {
    /// Identity whose memo store this memo belongs to
    #[arg(long = "for")]
    pub for_identity: String,

    /// Memo subject (shown in list one-liners)
    #[arg(long)]
    pub subject: String,

    /// Memo body, or @path to read body from a file
    #[arg(long)]
    pub body: Option<String>,

    /// Read memo body from a file
    #[arg(long)]
    pub body_file: Option<PathBuf>,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum MemoCommand {
    /// Save a memo for later reference
    Save(MemoSaveCommand),

    /// Delete a memo
    Delete {
        /// Memo handle or unambiguous prefix
        handle: String,

        /// Identity whose memo should be deleted
        #[arg(long = "for")]
        for_identity: String,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// List memos for an identity (one-liner subjects)
    List {
        /// Identity whose memos should be listed
        #[arg(long = "for")]
        for_identity: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Search memos by keyword in subject and body
    Search {
        /// Search query (case-insensitive substring)
        query: String,

        /// Identity whose memos should be searched
        #[arg(long = "for")]
        for_identity: String,

        /// Search subject only
        #[arg(long)]
        subject: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show a single memo in full detail
    Show {
        /// Memo handle or unambiguous prefix
        handle: String,

        /// Include thread context as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Seal a memo. Once absorbed, it can no longer be changed.
    Absorb(AbsorbCommand),

    /// Dump all memos for an identity
    Dump {
        /// Identity whose memos should be dumped (required)
        #[arg(long = "for")]
        for_identity: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Write dump output to a file instead of stdout
        #[arg(long)]
        output: Option<PathBuf>,

        /// Allow large dumps to stdout (over 25 records or 64 KiB)
        #[arg(long = "confirm-large")]
        confirm_large: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum NeedCommand {
    /// Send a need message into the recipient's Needs folder
    Send(super::NeedSendCommand),

    /// Wait for need events in the project-local mailspace
    Watch(Box<super::KindWatchCommand>),

    /// List needs for an identity or header filter
    #[command(group(
        clap::ArgGroup::new("list_scope")
            .required(true)
            .multiple(true)
            .args(["for_identity", "from", "to", "held"])
    ))]
    List {
        /// Identity whose needs should be listed
        #[arg(long = "for")]
        for_identity: Option<String>,

        /// List needs held under any other mailspace's address
        #[arg(long)]
        held: bool,

        /// Need creator identity or address filter
        #[arg(long)]
        from: Option<String>,

        /// Need owner identity or address filter
        #[arg(long)]
        to: Option<String>,

        /// Need folder status
        #[arg(long, default_value = "open")]
        status: TaskStatus,

        /// Keep needs that carry every named mark
        #[arg(long = "mark")]
        marks: Vec<String>,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Append the local pump's progress for each need when pump.db exists
        #[arg(long)]
        pump: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show a need message
    Show {
        /// Need handle or unambiguous prefix
        handle: String,

        /// Include thread context as JSON
        #[arg(long)]
        json: bool,

        /// Print only the newest message of the thread
        #[arg(long)]
        latest: bool,

        /// Print only the newest N messages of the thread, oldest first
        #[arg(long)]
        newest: Option<usize>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// List the pump's attempt records on a need thread
    Attempts {
        /// Need handle or unambiguous prefix
        handle: String,

        /// Output as one JSON object
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Dump local need messages for audit and board review
    Dump(TaskDumpCommand),

    /// Seal a need. Once absorbed, it can no longer be changed.
    Absorb(AbsorbCommand),

    /// Move a need from Needs to Done
    Done {
        /// Need handle or unambiguous prefix
        handle: String,

        /// Identity completing the need
        #[arg(long = "for")]
        for_identity: String,

        /// Optional completion note recorded in the mailspace event ledger
        #[arg(long)]
        note: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Move a need from Done back to Needs
    Reopen {
        /// Need handle or unambiguous prefix
        handle: String,

        /// Identity reopening the need
        #[arg(long = "for")]
        for_identity: String,

        /// Optional reopen note recorded in the mailspace event ledger
        #[arg(long)]
        note: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Bind unit tasks to a need; the need completes when all units land
    Bind {
        /// Need handle or unambiguous prefix
        handle: String,

        /// Unit task handles bound to the need (at least one)
        units: Vec<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Add marks to a need. Each slug is its own event.
    Mark(MarkCommand),

    /// Remove marks from a need. Each slug is its own event.
    Unmark(MarkCommand),
}

#[derive(Debug, Subcommand)]
pub enum WantCommand {
    /// Send a want message into the recipient's Wants folder
    Send(super::WantSendCommand),

    /// Wait for want events in the project-local mailspace
    Watch(Box<super::KindWatchCommand>),

    /// List wants for an identity or header filter
    #[command(group(
        clap::ArgGroup::new("list_scope")
            .required(true)
            .multiple(true)
            .args(["for_identity", "from", "to", "held"])
    ))]
    List {
        /// Identity whose wants should be listed
        #[arg(long = "for")]
        for_identity: Option<String>,

        /// List wants held under any other mailspace's address
        #[arg(long)]
        held: bool,

        /// Want creator identity or address filter
        #[arg(long)]
        from: Option<String>,

        /// Want owner identity or address filter
        #[arg(long)]
        to: Option<String>,

        /// Want folder status
        #[arg(long, default_value = "open")]
        status: WantStatus,

        /// Filter by repository metadata
        #[arg(long)]
        repo: Option<String>,

        /// Filter by lane metadata
        #[arg(long)]
        lane: Option<String>,

        /// Keep wants that carry every named mark
        #[arg(long = "mark")]
        marks: Vec<String>,

        /// Sort by comma-separated fields: priority,rank,created
        #[arg(long, default_value = "created")]
        sort: String,

        /// Output as JSON
        #[arg(long)]
        json: bool,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Show a want message
    Show {
        /// Want handle or unambiguous prefix
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

    /// Dump local want messages for audit and board review
    Dump(TaskDumpCommand),

    /// Seal a want. Once absorbed, it can no longer be changed.
    Absorb(AbsorbCommand),

    /// Set queryable priority and routing metadata on a want
    SetPriority {
        /// Want handle or unambiguous prefix
        handle: String,

        /// Identity whose want should be updated
        #[arg(long = "for")]
        for_identity: String,

        /// Priority label, such as P0, P1, P2
        #[arg(long)]
        priority: String,

        /// Numeric rank within the priority
        #[arg(long)]
        rank: Option<i64>,

        /// Repository this want belongs to
        #[arg(long)]
        repo: Option<String>,

        /// Work lane, such as correctness or docs
        #[arg(long)]
        lane: Option<String>,

        /// Claim this want blocks
        #[arg(long = "blocks-claim")]
        blocks_claim: Option<String>,

        /// Reason for the priority
        #[arg(long)]
        reason: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Promote a want from Wants to Needs
    Promote {
        /// Want handle or unambiguous prefix
        handle: String,

        /// Identity promoting the want
        #[arg(long = "for")]
        for_identity: String,

        /// Optional promotion note recorded in the mailspace event ledger
        #[arg(long)]
        note: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Move a want from Wants to Done
    Done {
        /// Want handle or unambiguous prefix
        handle: String,

        /// Identity closing the want
        #[arg(long = "for")]
        for_identity: String,

        /// Optional close note recorded in the mailspace event ledger
        #[arg(long)]
        note: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Alias for closing an obsolete want
    Drop {
        /// Want handle or unambiguous prefix
        handle: String,

        /// Identity closing the want
        #[arg(long = "for")]
        for_identity: String,

        /// Optional close note recorded in the mailspace event ledger
        #[arg(long)]
        note: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,
    },

    /// Add marks to a want. Each slug is its own event.
    Mark(MarkCommand),

    /// Remove marks from a want. Each slug is its own event.
    Unmark(MarkCommand),
}

#[derive(Debug, Clone, ValueEnum)]
pub enum TaskStatus {
    Open,
    Done,
    All,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum WantStatus {
    Open,
    Done,
    All,
}

#[derive(Debug, Clone, Args)]
pub struct MailDumpCommand {
    /// Identity whose local mailbox should be dumped
    #[arg(long = "for")]
    pub for_identity: Option<String>,

    /// Sender identity or address filter
    #[arg(long)]
    pub from: Option<String>,

    /// Recipient identity or address filter
    #[arg(long)]
    pub to: Option<String>,

    /// Identity or address involved as sender, recipient, or mailbox owner
    #[arg(long)]
    pub participant: Option<String>,

    /// Folder role to dump, or all mail folders
    #[arg(long, default_value = "all")]
    pub folder: String,

    /// Case-insensitive subject substring filter
    #[arg(long)]
    pub subject: Option<String>,

    /// Case-insensitive text body substring filter
    #[arg(long)]
    pub body: Option<String>,

    /// Absorb status filter
    #[arg(long, default_value = "all")]
    pub status: MailAbsorbStatus,

    /// Absorbing identity filter
    #[arg(long = "absorbed-by")]
    pub absorbed_by: Option<String>,

    /// Include messages on or after this time (RFC3339, YYYY-MM-DD, Nh, Nd, or Nw)
    #[arg(long)]
    pub since: Option<String>,

    /// Include messages before this time (RFC3339, YYYY-MM-DD, Nh, Nd, or Nw)
    #[arg(long)]
    pub before: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Write dump output to a file instead of stdout
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Allow large dumps to stdout (over 25 records or 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct TaskDumpCommand {
    /// Identity whose local task mailbox should be dumped
    #[arg(long = "for")]
    pub for_identity: Option<String>,

    /// Task creator identity or address filter
    #[arg(long)]
    pub from: Option<String>,

    /// Task owner identity or address filter
    #[arg(long)]
    pub to: Option<String>,

    /// Identity or address involved as creator, owner, or mailbox account
    #[arg(long)]
    pub participant: Option<String>,

    /// Task status to dump
    #[arg(long, default_value = "open")]
    pub status: TaskDumpStatusArg,

    /// Case-insensitive subject substring filter
    #[arg(long)]
    pub subject: Option<String>,

    /// Case-insensitive task body substring filter
    #[arg(long)]
    pub body: Option<String>,

    /// Include tasks on or after this time (RFC3339, YYYY-MM-DD, Nh, Nd, or Nw)
    #[arg(long)]
    pub since: Option<String>,

    /// Include tasks before this time (RFC3339, YYYY-MM-DD, Nh, Nd, or Nw)
    #[arg(long)]
    pub before: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Write dump output to a file instead of stdout
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Allow large dumps to stdout (over 25 records or 64 KiB)
    #[arg(long = "confirm-large")]
    pub confirm_large: bool,

    /// Project root to use
    #[arg(long)]
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum TaskDumpStatusArg {
    Open,
    Done,
    All,
}

/// Register and inspect project-local goal document paths.
#[derive(Debug, Subcommand)]
pub enum GoalCommand {
    /// Register a goal document path for Mind/campaign orientation
    Add {
        /// Path to the goal file (project-relative preferred)
        #[arg(long)]
        path: PathBuf,

        /// Optional short label for board display
        #[arg(long)]
        label: Option<String>,

        /// Identity registering the goal (metadata only)
        #[arg(long = "for")]
        for_identity: Option<String>,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// List registered goal paths
    List {
        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show one registered goal by handle or path
    Show {
        /// Goal handle, unambiguous prefix, or registered path
        selector: String,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Unregister a goal (does not delete the file)
    Drop {
        /// Goal handle, unambiguous prefix, or registered path
        selector: String,

        /// Project root to use
        #[arg(long)]
        project: Option<PathBuf>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}
